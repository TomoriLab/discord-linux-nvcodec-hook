use crate::ffi;
use std::borrow::Cow;
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const CHROMA_420: ffi::cudaVideoChromaFormat =
    ffi::cudaVideoChromaFormat_enum_cudaVideoChromaFormat_420;
const NV12: ffi::cudaVideoSurfaceFormat =
    ffi::cudaVideoSurfaceFormat_enum_cudaVideoSurfaceFormat_NV12;
const P016: ffi::cudaVideoSurfaceFormat =
    ffi::cudaVideoSurfaceFormat_enum_cudaVideoSurfaceFormat_P016;
const WEAVE: ffi::cudaVideoDeinterlaceMode =
    ffi::cudaVideoDeinterlaceMode_enum_cudaVideoDeinterlaceMode_Weave;

type FrameCallback = fn(*mut c_void, &[u8], i32, i32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Vp8,
    Av1,
    H264,
    Hevc,
}

impl Codec {
    pub fn from_webrtc(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Vp8),
            3 => Some(Self::Av1),
            4 => Some(Self::H264),
            5 => Some(Self::Hevc),
            _ => None,
        }
    }

    fn cuda(self) -> ffi::cudaVideoCodec {
        match self {
            Self::Vp8 => ffi::cudaVideoCodec_enum_cudaVideoCodec_VP8,
            Self::Av1 => ffi::cudaVideoCodec_enum_cudaVideoCodec_AV1,
            Self::H264 => ffi::cudaVideoCodec_enum_cudaVideoCodec_H264,
            Self::Hevc => ffi::cudaVideoCodec_enum_cudaVideoCodec_HEVC,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Vp8 => "VP8",
            Self::Av1 => "AV1",
            Self::H264 => "H264",
            Self::Hevc => "H265",
        }
    }
}

struct State {
    codec: Codec,
    cuda_context: ffi::CUcontext,
    parser: ffi::CUvideoparser,
    decoder: ffi::CUvideodecoder,
    output: Vec<u8>,
    raw: Vec<u8>,
    width: usize,
    height: usize,
    bit_depth_minus8: u8,
    callback: FrameCallback,
    callback_context: *mut c_void,
    failed: bool,
}

pub struct Backend {
    state: UnsafeCell<State>,
    busy: AtomicBool,
    trace_enabled: bool,
    traced_packets: AtomicUsize,
}

// Parser callbacks run synchronously within submit or flush while busy is held.
// The last Arc owns destruction, so state cannot be destroyed during a callback.
unsafe impl Send for Backend {}
unsafe impl Sync for Backend {}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct ContextGuard;

impl ContextGuard {
    fn push(context: ffi::CUcontext) -> Option<Self> {
        let result = unsafe { ffi::cuCtxPushCurrent_v2(context) };
        (result == 0).then_some(Self)
    }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        let mut previous = ptr::null_mut();
        unsafe { ffi::cuCtxPopCurrent_v2(&mut previous) };
    }
}

fn zeroed<T>() -> T {
    // The generated CUDA structs used here are plain C data with zero as a valid initial state.
    unsafe { std::mem::MaybeUninit::<T>::zeroed().assume_init() }
}

fn normalize_av1_packet(data: &[u8]) -> Result<Cow<'_, [u8]>, ()> {
    let mut offset = 0;
    while offset < data.len() {
        let header_offset = offset;
        let header = data[offset];
        if header & 0x81 != 0 || (header >> 3) & 0x0f == 0 {
            return Err(());
        }
        offset += 1;
        if header & 0x04 != 0 {
            if offset == data.len() {
                return Err(());
            }
            offset += 1;
        }
        if header & 0x02 == 0 {
            let mut normalized = Vec::with_capacity(data.len() + 8);
            normalized.extend_from_slice(&data[..header_offset]);
            normalized.push(header | 0x02);
            normalized.extend_from_slice(&data[header_offset + 1..offset]);
            let mut remaining = data.len() - offset;
            loop {
                let mut byte = (remaining & 0x7f) as u8;
                remaining >>= 7;
                if remaining != 0 {
                    byte |= 0x80;
                }
                normalized.push(byte);
                if remaining == 0 {
                    break;
                }
            }
            normalized.extend_from_slice(&data[offset..]);
            return Ok(Cow::Owned(normalized));
        }
        let mut payload_size = 0_u64;
        let mut complete = false;
        for byte_index in 0..8 {
            let Some(&byte) = data.get(offset) else {
                return Err(());
            };
            offset += 1;
            payload_size |= u64::from(byte & 0x7f) << (byte_index * 7);
            if byte & 0x80 == 0 {
                complete = true;
                break;
            }
        }
        if !complete || payload_size > (data.len() - offset) as u64 {
            return Err(());
        }
        offset += payload_size as usize;
    }
    Ok(Cow::Borrowed(data))
}

impl Backend {
    pub fn new(codec: Codec, callback: FrameCallback, context: *mut c_void) -> Option<Arc<Self>> {
        let backend = Arc::new(Self {
            state: UnsafeCell::new(State {
                codec,
                cuda_context: ptr::null_mut(),
                parser: ptr::null_mut(),
                decoder: ptr::null_mut(),
                output: Vec::new(),
                raw: Vec::new(),
                width: 0,
                height: 0,
                bit_depth_minus8: 0,
                callback,
                callback_context: context,
                failed: false,
            }),
            busy: AtomicBool::new(false),
            trace_enabled: std::env::var_os("DISCORD_NVDEC_TRACE").is_some(),
            traced_packets: AtomicUsize::new(0),
        });
        let state = unsafe { &mut *backend.state.get() };
        let mut device = 0;
        if unsafe { ffi::cuInit(0) } != 0
            || unsafe { ffi::cuDeviceGet(&mut device, 0) } != 0
            || unsafe { ffi::cuCtxCreate_v2(&mut state.cuda_context, 4, device) } != 0
        {
            eprintln!("discord-nvdec: CUDA context setup failed");
            return None;
        }

        let mut caps: ffi::CUVIDDECODECAPS = zeroed();
        caps.eCodecType = codec.cuda();
        caps.eChromaFormat = CHROMA_420;
        if unsafe { ffi::cuvidGetDecoderCaps(&mut caps) } != 0 || caps.bIsSupported == 0 {
            let mut previous = ptr::null_mut();
            unsafe { ffi::cuCtxPopCurrent_v2(&mut previous) };
            eprintln!("discord-nvdec: {} is unsupported by this GPU", codec.name());
            return None;
        }

        let mut parser: ffi::CUVIDPARSERPARAMS = zeroed();
        parser.CodecType = codec.cuda();
        parser.ulMaxNumDecodeSurfaces = 20;
        parser.ulClockRate = 90_000;
        parser.ulMaxDisplayDelay = 0;
        parser.pUserData = Arc::as_ptr(&backend).cast_mut().cast();
        parser.pfnSequenceCallback = Some(on_sequence);
        parser.pfnDecodePicture = Some(on_decode);
        parser.pfnDisplayPicture = Some(on_display);
        let result = unsafe { ffi::cuvidCreateVideoParser(&mut state.parser, &mut parser) };
        let mut previous = ptr::null_mut();
        unsafe { ffi::cuCtxPopCurrent_v2(&mut previous) };
        if result != 0 {
            eprintln!("discord-nvdec: cuvidCreateVideoParser failed: {result}");
            return None;
        }
        Some(backend)
    }

    pub fn submit(&self, data: &[u8], timestamp: u32, end_of_picture: bool) -> Result<(), ()> {
        if data.is_empty()
            || data.len() > u32::MAX as usize
            || self.busy.swap(true, Ordering::AcqRel)
        {
            return Err(());
        }
        let _busy = BusyGuard(&self.busy);
        let (parser, context, failed, codec) = unsafe {
            let state = &*self.state.get();
            (state.parser, state.cuda_context, state.failed, state.codec)
        };
        if failed {
            return Err(());
        }
        let payload = if codec == Codec::Av1 {
            normalize_av1_packet(data)?
        } else {
            Cow::Borrowed(data)
        };
        let _context = ContextGuard::push(context).ok_or(())?;
        let mut packet: ffi::CUVIDSOURCEDATAPACKET = zeroed();
        packet.flags = if end_of_picture { 0x02 | 0x08 } else { 0x02 };
        packet.payload = payload.as_ptr();
        packet.payload_size = payload.len() as _;
        packet.timestamp = timestamp as _;
        if self.trace_enabled
            && unsafe { &*self.state.get() }.codec == Codec::Av1
            && self.traced_packets.fetch_add(1, Ordering::Relaxed) < 4
        {
            let header = data
                .iter()
                .take(16)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            eprintln!(
                "discord-nvdec: AV1 packet size={} flags={:#x} normalized={} header={header}",
                data.len(),
                packet.flags,
                matches!(payload, Cow::Owned(_))
            );
        }
        let result = unsafe { ffi::cuvidParseVideoData(parser, &mut packet) };
        if result == 0 && !unsafe { &*self.state.get() }.failed {
            Ok(())
        } else {
            eprintln!("discord-nvdec: cuvidParseVideoData failed: {result}");
            Err(())
        }
    }

    pub fn flush(&self) -> Result<(), ()> {
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(());
        }
        let _busy = BusyGuard(&self.busy);
        let (parser, context) = unsafe {
            let state = &*self.state.get();
            (state.parser, state.cuda_context)
        };
        let _context = ContextGuard::push(context).ok_or(())?;
        let mut packet: ffi::CUVIDSOURCEDATAPACKET = zeroed();
        packet.flags = 0x01;
        let result = unsafe { ffi::cuvidParseVideoData(parser, &mut packet) };
        if result == 0 && !unsafe { &*self.state.get() }.failed {
            Ok(())
        } else {
            Err(())
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        if state.cuda_context.is_null() {
            return;
        }
        if let Some(_context) = ContextGuard::push(state.cuda_context) {
            if !state.parser.is_null() {
                unsafe { ffi::cuvidDestroyVideoParser(state.parser) };
            }
            if !state.decoder.is_null() {
                unsafe { ffi::cuvidDestroyDecoder(state.decoder) };
            }
        }
        unsafe { ffi::cuCtxDestroy_v2(state.cuda_context) };
    }
}

unsafe extern "C" fn on_sequence(context: *mut c_void, format: *mut ffi::CUVIDEOFORMAT) -> i32 {
    if context.is_null() || format.is_null() {
        return 0;
    }
    let backend = unsafe { &*(context as *const Backend) };
    let state = unsafe { &mut *backend.state.get() };
    let format = unsafe { &*format };
    let width = format.display_area.right - format.display_area.left;
    let height = format.display_area.bottom - format.display_area.top;
    if format.codec != state.codec.cuda()
        || format.chroma_format != CHROMA_420
        || !matches!(format.bit_depth_luma_minus8, 0 | 2 | 4)
        || format.progressive_sequence == 0
        || width <= 0
        || height <= 0
        || width & 1 != 0
        || height & 1 != 0
        || width > 8192
        || height > 8192
        || format.display_area.left < 0
        || format.display_area.top < 0
        || format.display_area.right > format.coded_width as i32
        || format.display_area.bottom > format.coded_height as i32
    {
        if backend.trace_enabled {
            eprintln!(
                "discord-nvdec: sequence rejected codec={} chroma={} depth={} progressive={} coded={}x{} display=({},{})-({},{})",
                format.codec,
                format.chroma_format,
                format.bit_depth_luma_minus8,
                format.progressive_sequence,
                format.coded_width,
                format.coded_height,
                format.display_area.left,
                format.display_area.top,
                format.display_area.right,
                format.display_area.bottom
            );
        }
        state.failed = true;
        return 0;
    }

    let mut caps: ffi::CUVIDDECODECAPS = zeroed();
    caps.eCodecType = state.codec.cuda();
    caps.eChromaFormat = CHROMA_420;
    caps.nBitDepthMinus8 = format.bit_depth_luma_minus8 as _;
    if unsafe { ffi::cuvidGetDecoderCaps(&mut caps) } != 0
        || caps.bIsSupported == 0
        || format.coded_width < caps.nMinWidth as u32
        || format.coded_height < caps.nMinHeight as u32
        || format.coded_width > caps.nMaxWidth
        || format.coded_height > caps.nMaxHeight
    {
        if backend.trace_enabled {
            eprintln!(
                "discord-nvdec: decoder caps rejected codec={} depth={} coded={}x{} supported={} limits={}x{}..{}x{}",
                format.codec,
                format.bit_depth_luma_minus8,
                format.coded_width,
                format.coded_height,
                caps.bIsSupported,
                caps.nMinWidth,
                caps.nMinHeight,
                caps.nMaxWidth,
                caps.nMaxHeight
            );
        }
        state.failed = true;
        return 0;
    }

    if !state.decoder.is_null() {
        if unsafe { ffi::cuvidDestroyDecoder(state.decoder) } != 0 {
            state.failed = true;
            return 0;
        }
        state.decoder = ptr::null_mut();
    }

    let mut info: ffi::CUVIDDECODECREATEINFO = zeroed();
    info.CodecType = state.codec.cuda();
    info.ChromaFormat = CHROMA_420;
    info.bitDepthMinus8 = format.bit_depth_luma_minus8 as _;
    info.OutputFormat = if info.bitDepthMinus8 == 0 { NV12 } else { P016 };
    if caps.nOutputFormatMask & (1 << info.OutputFormat) == 0 {
        state.failed = true;
        return 0;
    }
    info.DeinterlaceMode = WEAVE;
    info.ulWidth = format.coded_width as _;
    info.ulHeight = format.coded_height as _;
    info.ulMaxWidth = info.ulWidth;
    info.ulMaxHeight = info.ulHeight;
    info.ulNumDecodeSurfaces = format.min_num_decode_surfaces as _;
    info.ulNumOutputSurfaces = 2;
    info.ulTargetWidth = width as _;
    info.ulTargetHeight = height as _;
    info.display_area.left = format.display_area.left as _;
    info.display_area.top = format.display_area.top as _;
    info.display_area.right = format.display_area.right as _;
    info.display_area.bottom = format.display_area.bottom as _;
    let result = unsafe { ffi::cuvidCreateDecoder(&mut state.decoder, &mut info) };
    if result != 0 {
        eprintln!("discord-nvdec: cuvidCreateDecoder failed: {result}");
        state.failed = true;
        return 0;
    }
    state.width = width as usize;
    state.height = height as usize;
    state.bit_depth_minus8 = format.bit_depth_luma_minus8;
    info.ulNumDecodeSurfaces as i32
}

unsafe extern "C" fn on_decode(context: *mut c_void, picture: *mut ffi::CUVIDPICPARAMS) -> i32 {
    if context.is_null() || picture.is_null() {
        return 0;
    }
    let backend = unsafe { &*(context as *const Backend) };
    let state = unsafe { &mut *backend.state.get() };
    let result = unsafe { ffi::cuvidDecodePicture(state.decoder, picture) };
    if result != 0 {
        state.failed = true;
        eprintln!("discord-nvdec: cuvidDecodePicture failed: {result}");
        return 0;
    }
    1
}

unsafe extern "C" fn on_display(
    context: *mut c_void,
    display: *mut ffi::CUVIDPARSERDISPINFO,
) -> i32 {
    if context.is_null() || display.is_null() {
        return 0;
    }
    let backend = unsafe { &*(context as *const Backend) };
    let display = unsafe { &*display };
    let (frame, callback, callback_context, width, height) = {
        let state = unsafe { &mut *backend.state.get() };
        let mut process: ffi::CUVIDPROCPARAMS = zeroed();
        process.progressive_frame = display.progressive_frame;
        process.top_field_first = display.top_field_first;
        let mut device_pointer = 0;
        let mut device_pitch = 0;
        let result = unsafe {
            ffi::cuvidMapVideoFrame64(
                state.decoder,
                display.picture_index,
                &mut device_pointer,
                &mut device_pitch,
                &mut process,
            )
        };
        if result != 0 {
            state.failed = true;
            return 0;
        }
        let copied = copy_mapped_frame(state, device_pointer, device_pitch as usize);
        let unmap_result = unsafe { ffi::cuvidUnmapVideoFrame64(state.decoder, device_pointer) };
        if copied.is_err() || unmap_result != 0 {
            state.failed = true;
            return 0;
        }
        (
            std::mem::take(&mut state.output),
            state.callback,
            state.callback_context,
            state.width as i32,
            state.height as i32,
        )
    };
    callback(
        callback_context,
        &frame,
        width,
        height,
        display.timestamp as u32,
    );
    unsafe { &mut *backend.state.get() }.output = frame;
    1
}

fn copy_mapped_frame(state: &mut State, source: u64, source_pitch: usize) -> Result<(), ()> {
    let frame_size = state
        .width
        .checked_mul(state.height)
        .and_then(|size| size.checked_mul(3))
        .map(|size| size / 2)
        .ok_or(())?;
    let bytes_per_sample = if state.bit_depth_minus8 == 0 { 1 } else { 2 };
    let raw_size = frame_size.checked_mul(bytes_per_sample).ok_or(())?;
    state.output.resize(frame_size, 0);
    let target = if bytes_per_sample == 1 {
        &mut state.output
    } else {
        state.raw.resize(raw_size, 0);
        &mut state.raw
    };
    let row_width = state.width.checked_mul(bytes_per_sample).ok_or(())?;
    for plane in 0..2 {
        let offset = if plane == 0 {
            0
        } else {
            state.width * state.height * bytes_per_sample
        };
        let mut copy: ffi::CUDA_MEMCPY2D = zeroed();
        copy.srcMemoryType = ffi::CUmemorytype_enum_CU_MEMORYTYPE_DEVICE;
        let source_offset = source_pitch
            .checked_mul(state.height)
            .and_then(|size| size.checked_mul(plane))
            .ok_or(())?;
        copy.srcDevice = source.checked_add(source_offset as u64).ok_or(())?;
        copy.srcPitch = source_pitch;
        copy.dstMemoryType = ffi::CUmemorytype_enum_CU_MEMORYTYPE_HOST;
        copy.dstHost = unsafe { target.as_mut_ptr().add(offset) }.cast();
        copy.dstPitch = row_width;
        copy.WidthInBytes = row_width;
        copy.Height = if plane == 0 {
            state.height
        } else {
            state.height / 2
        };
        if unsafe { ffi::cuMemcpy2D_v2(&copy) } != 0 {
            return Err(());
        }
    }
    if bytes_per_sample == 2 {
        for (index, sample) in state.raw.as_chunks::<2>().0.iter().enumerate() {
            state.output[index] = sample[1];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Backend, Codec};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FrameResult {
        count: AtomicUsize,
        invalid: AtomicBool,
    }

    fn on_frame(context: *mut c_void, frame: &[u8], width: i32, height: i32, _: u32) {
        let result = unsafe { &*(context as *const FrameResult) };
        if (width, height) != (256, 144) || frame.len() != 256 * 144 * 3 / 2 {
            result.invalid.store(true, Ordering::Relaxed);
        }
        result.count.fetch_add(1, Ordering::Relaxed);
    }

    fn decode_stream(codec: Codec, packets: impl IntoIterator<Item = (Vec<u8>, u32, bool)>) {
        let result = FrameResult {
            count: AtomicUsize::new(0),
            invalid: AtomicBool::new(false),
        };
        let backend = Backend::new(
            codec,
            on_frame,
            (&result as *const FrameResult).cast_mut().cast(),
        )
        .expect("GPU codec support");
        for (packet, timestamp, end_of_picture) in packets {
            backend.submit(&packet, timestamp, end_of_picture).unwrap();
        }
        backend.flush().unwrap();
        assert_eq!(result.count.load(Ordering::Relaxed), 2);
        assert!(!result.invalid.load(Ordering::Relaxed));
    }

    #[test]
    fn maps_discord_video_codecs() {
        assert_eq!(Codec::from_webrtc(1), Some(Codec::Vp8));
        assert_eq!(Codec::from_webrtc(3), Some(Codec::Av1));
        assert_eq!(Codec::from_webrtc(4), Some(Codec::H264));
        assert_eq!(Codec::from_webrtc(5), Some(Codec::Hevc));
        assert_eq!(Codec::from_webrtc(2), None);
    }

    #[test]
    fn decodes_hevc_main() {
        decode_stream(
            Codec::Hevc,
            [(
                include_bytes!("../tests/fixtures/hevc-main.h265").to_vec(),
                1234,
                false,
            )],
        );
    }

    #[test]
    fn decodes_hevc_main10() {
        decode_stream(
            Codec::Hevc,
            [(
                include_bytes!("../tests/fixtures/hevc-main10.h265").to_vec(),
                1234,
                false,
            )],
        );
    }

    #[test]
    fn decodes_h264() {
        decode_stream(
            Codec::H264,
            [(
                include_bytes!("../tests/fixtures/h264.h264").to_vec(),
                1234,
                false,
            )],
        );
    }

    #[test]
    fn decodes_av1() {
        let video = include_bytes!("../tests/fixtures/av1.ivf");
        assert_eq!(&video[..4], b"DKIF");
        let mut offset = 32;
        let mut packets = Vec::new();
        while offset + 12 <= video.len() {
            let size = u32::from_le_bytes(video[offset..offset + 4].try_into().unwrap()) as usize;
            let timestamp = u64::from_le_bytes(video[offset + 4..offset + 12].try_into().unwrap());
            offset += 12;
            packets.push((
                video[offset..offset + size].to_vec(),
                timestamp as u32,
                true,
            ));
            offset += size;
        }
        assert_eq!(offset, video.len());
        decode_stream(Codec::Av1, packets);
    }

    #[test]
    fn decodes_batched_av1_stream() {
        decode_stream(
            Codec::Av1,
            [(
                include_bytes!("../tests/fixtures/av1.obu").to_vec(),
                1234,
                false,
            )],
        );
    }

    #[test]
    fn decodes_av1_packet_without_last_obu_size() {
        let video = include_bytes!("../tests/fixtures/av1.ivf");
        let size = u32::from_le_bytes(video[32..36].try_into().unwrap()) as usize;
        let frame = &video[44..44 + size];
        assert_eq!(&frame[..2], &[0x12, 0]);
        assert_eq!(frame[15], 0x32);
        let mut packet = frame[2..16].to_vec();
        packet[13] = 0x30;
        packet.extend_from_slice(&frame[18..]);
        let result = FrameResult {
            count: AtomicUsize::new(0),
            invalid: AtomicBool::new(false),
        };
        let backend = Backend::new(
            Codec::Av1,
            on_frame,
            (&result as *const FrameResult).cast_mut().cast(),
        )
        .unwrap();
        backend.submit(&packet, 1234, true).unwrap();
        backend.flush().unwrap();
        assert_eq!(result.count.load(Ordering::Relaxed), 1);
        assert!(!result.invalid.load(Ordering::Relaxed));
    }

    #[test]
    fn decodes_vp8() {
        let video = include_bytes!("../tests/fixtures/vp8.ivf");
        assert_eq!(&video[..4], b"DKIF");
        let mut offset = 32;
        let mut packets = Vec::new();
        while offset + 12 <= video.len() {
            let size = u32::from_le_bytes(video[offset..offset + 4].try_into().unwrap()) as usize;
            let timestamp = u64::from_le_bytes(video[offset + 4..offset + 12].try_into().unwrap());
            offset += 12;
            packets.push((
                video[offset..offset + size].to_vec(),
                timestamp as u32,
                true,
            ));
            offset += size;
        }
        assert_eq!(offset, video.len());
        decode_stream(Codec::Vp8, packets);
    }
}

use crate::ffi;
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const HEVC: ffi::cudaVideoCodec = ffi::cudaVideoCodec_enum_cudaVideoCodec_HEVC;
const CHROMA_420: ffi::cudaVideoChromaFormat =
    ffi::cudaVideoChromaFormat_enum_cudaVideoChromaFormat_420;
const NV12: ffi::cudaVideoSurfaceFormat =
    ffi::cudaVideoSurfaceFormat_enum_cudaVideoSurfaceFormat_NV12;
const P016: ffi::cudaVideoSurfaceFormat =
    ffi::cudaVideoSurfaceFormat_enum_cudaVideoSurfaceFormat_P016;
const WEAVE: ffi::cudaVideoDeinterlaceMode =
    ffi::cudaVideoDeinterlaceMode_enum_cudaVideoDeinterlaceMode_Weave;

type FrameCallback = fn(*mut c_void, &[u8], i32, i32, u32);

struct State {
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

impl Backend {
    pub fn new(callback: FrameCallback, context: *mut c_void) -> Option<Arc<Self>> {
        let backend = Arc::new(Self {
            state: UnsafeCell::new(State {
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

        let mut parser: ffi::CUVIDPARSERPARAMS = zeroed();
        parser.CodecType = HEVC;
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

    pub fn submit(&self, data: &[u8], timestamp: u32) -> Result<(), ()> {
        if data.is_empty()
            || data.len() > u32::MAX as usize
            || self.busy.swap(true, Ordering::AcqRel)
        {
            return Err(());
        }
        let _busy = BusyGuard(&self.busy);
        let (parser, context, failed) = unsafe {
            let state = &*self.state.get();
            (state.parser, state.cuda_context, state.failed)
        };
        if failed {
            return Err(());
        }
        let _context = ContextGuard::push(context).ok_or(())?;
        let mut packet: ffi::CUVIDSOURCEDATAPACKET = zeroed();
        packet.flags = 0x02;
        packet.payload = data.as_ptr();
        packet.payload_size = data.len() as _;
        packet.timestamp = timestamp as _;
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
    if format.chroma_format != CHROMA_420
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
        state.failed = true;
        return 0;
    }

    let mut caps: ffi::CUVIDDECODECAPS = zeroed();
    caps.eCodecType = HEVC;
    caps.eChromaFormat = CHROMA_420;
    caps.nBitDepthMinus8 = format.bit_depth_luma_minus8 as _;
    if unsafe { ffi::cuvidGetDecoderCaps(&mut caps) } != 0
        || caps.bIsSupported == 0
        || format.coded_width < caps.nMinWidth as u32
        || format.coded_height < caps.nMinHeight as u32
        || format.coded_width > caps.nMaxWidth
        || format.coded_height > caps.nMaxHeight
    {
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
    info.CodecType = HEVC;
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
    use super::Backend;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn on_frame(_: *mut c_void, frame: &[u8], width: i32, height: i32, _: u32) {
        assert_eq!((width, height), (256, 144));
        assert_eq!(frame.len(), 256 * 144 * 3 / 2);
        FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn decodes_hevc_main() {
        FRAME_COUNT.store(0, Ordering::Relaxed);
        let video = include_bytes!("../tests/fixtures/hevc-main.h265");
        let backend = Backend::new(on_frame, std::ptr::null_mut()).unwrap();
        backend.submit(video, 1234).unwrap();
        backend.flush().unwrap();
        assert_eq!(FRAME_COUNT.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn decodes_hevc_main10() {
        FRAME_COUNT.store(0, Ordering::Relaxed);
        let video = include_bytes!("../tests/fixtures/hevc-main10.h265");
        let backend = Backend::new(on_frame, std::ptr::null_mut()).unwrap();
        backend.submit(video, 1234).unwrap();
        backend.flush().unwrap();
        assert_eq!(FRAME_COUNT.load(Ordering::Relaxed), 2);
    }
}

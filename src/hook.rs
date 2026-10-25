use crate::elf::{self, Symbols};
use crate::nvdec::{Backend, Codec};
use std::ffi::OsStr;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

const PATCH_LENGTH: usize = 14;
const EXPECTED_PROLOGUE: [u8; PATCH_LENGTH] = [
    0x55, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x53, 0x48, 0x83, 0xec, 0x38,
];

type Dlopen = unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void;
type FactoryCreate = unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void) -> *mut c_void;
type CodecType = unsafe extern "C" fn(*const c_void) -> c_int;

static REAL_DLOPEN: OnceLock<Option<Dlopen>> = OnceLock::new();
static DECODER_VTABLE: OnceLock<[usize; 10]> = OnceLock::new();
static HOOK_LOCK: Mutex<()> = Mutex::new(());
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static ORIGINAL_CREATE: AtomicUsize = AtomicUsize::new(0);
static ADDRESSES: OnceLock<Addresses> = OnceLock::new();

struct Addresses {
    base: usize,
    symbols: Symbols,
}

impl Addresses {
    fn get(&self, name: &'static str) -> usize {
        self.base + self.symbols.get(name).expect("required symbol").offset
    }
}

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        file_descriptor: c_int,
        offset: isize,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
    fn mprotect(address: *mut c_void, length: usize, protection: c_int) -> c_int;
    fn getpagesize() -> c_int;
}

#[repr(C)]
struct Decoder {
    vtable: *const usize,
    codec: Codec,
    state: Mutex<DecoderState>,
    callback: Arc<DecoderCallback>,
}

struct DecoderState {
    backend: Option<Arc<Backend>>,
}

struct DecoderCallback {
    callback: AtomicUsize,
}

#[repr(align(16))]
struct Aligned<const SIZE: usize>([u8; SIZE]);

fn module_location(module_path: &PathBuf) -> Option<usize> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let address = fields.next()?;
        fields.next()?;
        let offset = fields.next()?;
        fields.next()?;
        fields.next()?;
        let path = fields.next().unwrap_or_default();
        if offset == "00000000" && Path::new(path) == module_path {
            let start = address.split_once('-')?.0;
            return usize::from_str_radix(start, 16).ok();
        }
    }
    None
}

unsafe fn write_absolute_jump(destination: *mut u8, target: usize) {
    let instruction = [0xff, 0x25, 0, 0, 0, 0];
    unsafe {
        ptr::copy_nonoverlapping(instruction.as_ptr(), destination, instruction.len());
        ptr::copy_nonoverlapping(
            target.to_ne_bytes().as_ptr(),
            destination.add(instruction.len()),
            mem::size_of::<usize>(),
        );
    }
}

unsafe fn patch_bytes(address: usize, expected: &[u8], replacement: &[u8]) -> bool {
    if expected.is_empty()
        || expected.len() != replacement.len()
        || unsafe { std::slice::from_raw_parts(address as *const u8, expected.len()) } != expected
    {
        return false;
    }
    let page_size = unsafe { getpagesize() } as usize;
    if page_size == 0 {
        return false;
    }
    let page = address & !(page_size - 1);
    let Some(last_address) = address.checked_add(expected.len() - 1) else {
        return false;
    };
    let last_page = last_address & !(page_size - 1);
    let Some(length) = last_page
        .checked_sub(page)
        .and_then(|value| value.checked_add(page_size))
    else {
        return false;
    };
    if unsafe { mprotect(page as *mut c_void, length, 7) } != 0 {
        return false;
    }
    unsafe {
        ptr::copy_nonoverlapping(replacement.as_ptr(), address as *mut u8, replacement.len())
    };
    (unsafe { mprotect(page as *mut c_void, length, 5) }) == 0
}

fn encoder_call(addresses: &Addresses) -> Option<(usize, [u8; 5])> {
    let symbol = addresses.symbols.get(elf::ENCODER_FACTORY)?;
    if symbol.size < 32 || symbol.size > 8192 {
        return None;
    }
    let start = addresses.get(elf::ENCODER_FACTORY);
    let code = unsafe { std::slice::from_raw_parts(start as *const u8, symbol.size) };
    let mut match_found = None;
    for index in 0..code.len().saturating_sub(40) {
        if code.get(index..index + 6) != Some(&[0xbe, 10, 0, 0, 0, 0xe8]) {
            continue;
        }
        let first_call = index + 5;
        let first_displacement =
            i32::from_le_bytes(code.get(first_call + 1..first_call + 5)?.try_into().ok()?);
        let first_target = (first_call + 5) as isize + first_displacement as isize;
        let Some(second) = (first_call + 5..(first_call + 40).min(code.len() - 7))
            .find(|position| code.get(*position..*position + 3) == Some(&[0x31, 0xf6, 0xe8]))
        else {
            continue;
        };
        let second_call = second + 2;
        let second_displacement = i32::from_le_bytes(
            code.get(second_call + 1..second_call + 5)?
                .try_into()
                .ok()?,
        );
        let second_target = (second_call + 5) as isize + second_displacement as isize;
        let helper = addresses.symbols.get(elf::ENCODER_ADD_HELPER)?;
        if first_target != second_target
            || first_target != helper.offset as isize - symbol.offset as isize
        {
            continue;
        }
        let bytes: [u8; 5] = code.get(first_call..first_call + 5)?.try_into().ok()?;
        if match_found.replace((start + first_call, bytes)).is_some() {
            return None;
        }
    }
    match_found
}

fn install_hook(path: &PathBuf) {
    let Ok(_lock) = HOOK_LOCK.lock() else {
        return;
    };
    if HOOK_INSTALLED.load(Ordering::Acquire) {
        return;
    }
    let Some(base) = module_location(path) else {
        return;
    };
    let Some(symbols) = elf::read_symbols(path) else {
        eprintln!("discord-nvdec: required ELF symbols missing; hook skipped");
        return;
    };
    let addresses = Addresses { base, symbols };
    let decoder_symbol = addresses
        .symbols
        .get(elf::DECODER_FACTORY)
        .expect("required symbol");
    if decoder_symbol.size < PATCH_LENGTH || decoder_symbol.size > 8192 {
        eprintln!("discord-nvdec: decoder factory size mismatch; hook skipped");
        return;
    }
    let target = addresses.get(elf::DECODER_FACTORY);
    let Some((encoder_address, encoder_expected)) = encoder_call(&addresses) else {
        eprintln!("discord-nvdec: encoder call pattern mismatch; hook skipped");
        return;
    };
    let prologue = unsafe { std::slice::from_raw_parts(target as *const u8, PATCH_LENGTH) };
    if prologue != EXPECTED_PROLOGUE {
        eprintln!("discord-nvdec: voice module signature mismatch; hook skipped");
        return;
    }

    let trampoline = unsafe { mmap(ptr::null_mut(), 28, 3, 0x22, -1, 0) };
    if trampoline as isize == -1 {
        return;
    }
    unsafe {
        ptr::copy_nonoverlapping(target as *const u8, trampoline.cast(), PATCH_LENGTH);
        write_absolute_jump(
            (trampoline as *mut u8).add(PATCH_LENGTH),
            target + PATCH_LENGTH,
        );
    }
    if unsafe { mprotect(trampoline, 28, 5) } != 0 {
        unsafe { munmap(trampoline, 28) };
        return;
    }

    let page_size = unsafe { getpagesize() } as usize;
    let page = target & !(page_size - 1);
    if unsafe { mprotect(page as *mut c_void, page_size, 7) } != 0 {
        unsafe { munmap(trampoline, 28) };
        return;
    }
    if ADDRESSES.set(addresses).is_err() {
        unsafe { munmap(trampoline, 28) };
        let _ = unsafe { mprotect(page as *mut c_void, page_size, 5) };
        return;
    }
    ORIGINAL_CREATE.store(trampoline as usize, Ordering::Release);
    unsafe { write_absolute_jump(target as *mut u8, hooked_create as *const () as usize) };
    let _ = unsafe { mprotect(page as *mut c_void, page_size, 5) };
    HOOK_INSTALLED.store(true, Ordering::Release);

    let no_operation = [0x90; 5];
    if unsafe { patch_bytes(encoder_address, &encoder_expected, &no_operation) } {
        eprintln!("discord-nvdec: CUDA NVENC preference patch installed");
    } else {
        eprintln!("discord-nvdec: CUDA NVENC preference signature mismatch");
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void {
    let Some(real_dlopen) = *REAL_DLOPEN.get_or_init(|| unsafe {
        let symbol = dlsym(usize::MAX as *mut c_void, c"dlopen".as_ptr());
        mem::transmute::<*mut c_void, Option<Dlopen>>(symbol)
    }) else {
        return ptr::null_mut();
    };
    let handle = unsafe { real_dlopen(filename, flags) };
    if !handle.is_null() && !filename.is_null() {
        let name = unsafe { CStr::from_ptr(filename) };
        if name.to_bytes().ends_with(b"discord_voice.node") {
            let path = PathBuf::from(OsStr::from_bytes(name.to_bytes()));
            if let Ok(path) = path.canonicalize() {
                install_hook(&path);
            }
        }
    }
    handle
}

unsafe extern "C" fn hooked_create(
    factory: *mut c_void,
    environment: *const c_void,
    format: *const c_void,
) -> *mut c_void {
    let Some(addresses) = ADDRESSES.get() else {
        return ptr::null_mut();
    };
    let codec_type: CodecType = unsafe { mem::transmute(addresses.get(elf::CODEC_TYPE)) };
    if let Some(codec) = Codec::from_webrtc(unsafe { codec_type(format) }) {
        let decoder = Box::new(Decoder {
            vtable: DECODER_VTABLE
                .get_or_init(|| {
                    [
                        decoder_destroy as *const () as usize,
                        decoder_delete as *const () as usize,
                        decoder_configure as *const () as usize,
                        decoder_decode_legacy as *const () as usize,
                        decoder_decode as *const () as usize,
                        decoder_register_callback as *const () as usize,
                        decoder_release as *const () as usize,
                        addresses.get(elf::HARDWARE_INFO),
                        decoder_implementation_name as *const () as usize,
                        decoder_set_stream_id as *const () as usize,
                    ]
                })
                .as_ptr(),
            codec,
            state: Mutex::new(DecoderState { backend: None }),
            callback: Arc::new(DecoderCallback {
                callback: AtomicUsize::new(0),
            }),
        });
        if let Some(backend) = Backend::new(codec, on_nvdec_frame, Arc::clone(&decoder.callback)) {
            decoder.state.lock().expect("new decoder state").backend = Some(backend);
            eprintln!(
                "discord-nvdec: {} selected for NVDEC decoding",
                codec.name()
            );
            return Box::into_raw(decoder).cast();
        }
    }
    let original: FactoryCreate =
        unsafe { mem::transmute(ORIGINAL_CREATE.load(Ordering::Acquire)) };
    unsafe { original(factory, environment, format) }
}

unsafe extern "C" fn decoder_destroy(decoder: *mut Decoder) {
    if let Some(decoder) = unsafe { decoder.as_ref() }
        && let Ok(mut state) = decoder.state.lock()
    {
        state.backend = None;
    }
}

unsafe extern "C" fn decoder_delete(decoder: *mut Decoder) {
    if !decoder.is_null() {
        let decoder = unsafe { Box::from_raw(decoder) };
        decoder.callback.callback.store(0, Ordering::Release);
        drop(decoder);
    }
}

unsafe extern "C" fn decoder_configure(decoder: *mut Decoder, settings: *const u8) -> bool {
    if decoder.is_null() || settings.is_null() {
        return false;
    }
    let codec_type = unsafe { ptr::read_unaligned(settings.add(0x14).cast::<i32>()) };
    let decoder = unsafe { &*decoder };
    if Codec::from_webrtc(codec_type) != Some(decoder.codec) {
        return false;
    }
    let Ok(mut state) = decoder.state.lock() else {
        return false;
    };
    if state.backend.is_none() {
        state.backend = Backend::new(decoder.codec, on_nvdec_frame, Arc::clone(&decoder.callback));
    }
    state.backend.is_some()
}

unsafe extern "C" fn decoder_decode(
    decoder: *mut Decoder,
    encoded: *const u8,
    _missing_frames: bool,
    _render_time: i64,
) -> i32 {
    if decoder.is_null() || encoded.is_null() {
        return -1;
    }
    let image_buffer = unsafe { ptr::read_unaligned(encoded.add(0x88).cast::<*mut c_void>()) };
    let image_size = unsafe { ptr::read_unaligned(encoded.add(0x90).cast::<usize>()) };
    let timestamp = unsafe { ptr::read_unaligned(encoded.add(0x98).cast::<u32>()) };
    if image_buffer.is_null() || image_size == 0 || image_size > 32 * 1024 * 1024 {
        return -1;
    }
    let image_vtable = unsafe { *(image_buffer as *const *const usize) };
    let image_data: unsafe extern "C" fn(*mut c_void) -> *const u8 =
        unsafe { mem::transmute(*image_vtable.add(4)) };
    let image_data = unsafe { image_data(image_buffer) };
    if image_data.is_null() {
        return -1;
    }
    let image = unsafe { std::slice::from_raw_parts(image_data, image_size) };
    let backend = unsafe { &*decoder }
        .state
        .lock()
        .ok()
        .and_then(|state| state.backend.clone());
    match backend {
        Some(backend) if backend.submit(image, timestamp, true).is_ok() => 0,
        _ => -1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn discord_nvdec_flush_decoder(decoder: *const c_void) -> i32 {
    let backend = unsafe { decoder.cast::<Decoder>().as_ref() }
        .and_then(|decoder| decoder.state.lock().ok())
        .and_then(|state| state.backend.clone());
    match backend {
        Some(backend) if backend.flush().is_ok() => 0,
        _ => -1,
    }
}

unsafe extern "C" fn decoder_decode_legacy(
    decoder: *mut Decoder,
    encoded: *const u8,
    render_time: i64,
) -> i32 {
    unsafe { decoder_decode(decoder, encoded, false, render_time) }
}

unsafe extern "C" fn decoder_register_callback(
    decoder: *mut Decoder,
    callback: *mut c_void,
) -> i32 {
    let Some(decoder) = (unsafe { decoder.as_ref() }) else {
        return -1;
    };
    decoder
        .callback
        .callback
        .store(callback as usize, Ordering::Release);
    0
}

unsafe extern "C" fn decoder_release(decoder: *mut Decoder) -> i32 {
    let Some(decoder) = (unsafe { decoder.as_ref() }) else {
        return -1;
    };
    let Ok(mut state) = decoder.state.lock() else {
        return -1;
    };
    state.backend = None;
    0
}

unsafe extern "C" fn decoder_implementation_name(_: *const Decoder) -> *const c_char {
    c"nvdec".as_ptr()
}

unsafe extern "C" fn decoder_set_stream_id(_: *mut Decoder, _: *const c_void) {}

fn nv12_to_i420(nv12: &[u8], width: usize, height: usize) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let y_size = width.checked_mul(height)?;
    let chroma_size = y_size / 4;
    if !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || nv12.len() != y_size + 2 * chroma_size
    {
        return None;
    }
    let y = nv12[..y_size].to_vec();
    let mut u = Vec::with_capacity(chroma_size);
    let mut v = Vec::with_capacity(chroma_size);
    for pair in nv12[y_size..].as_chunks::<2>().0 {
        u.push(pair[0]);
        v.push(pair[1]);
    }
    Some((y, u, v))
}

fn on_nvdec_frame(context: *mut c_void, nv12: &[u8], width: i32, height: i32, timestamp: u32) {
    if context.is_null() || width <= 0 || height <= 0 {
        return;
    }
    let callback = unsafe { &*context.cast::<DecoderCallback>() }
        .callback
        .load(Ordering::Acquire) as *mut c_void;
    if callback.is_null() {
        return;
    }
    let Some((y, u, v)) = nv12_to_i420(nv12, width as usize, height as usize) else {
        return;
    };
    unsafe { deliver_frame(callback, width, height, timestamp, &y, &u, &v) };
}

unsafe fn deliver_frame(
    callback: *mut c_void,
    width: i32,
    height: i32,
    timestamp: u32,
    y: &[u8],
    u: &[u8],
    v: &[u8],
) {
    let Some(addresses) = ADDRESSES.get() else {
        return;
    };
    type CreateBuffer = unsafe extern "C" fn(*mut *mut c_void, i32, i32) -> *mut c_void;
    type BufferData = unsafe extern "C" fn(*mut c_void) -> *mut u8;
    type BufferStride = unsafe extern "C" fn(*const c_void) -> i32;
    let create: CreateBuffer = unsafe { mem::transmute(addresses.get(elf::BUFFER_CREATE)) };
    let mut buffer = ptr::null_mut();
    unsafe { create(&mut buffer, width, height) };
    if buffer.is_null() {
        return;
    }
    for (source, stride_symbol, data_symbol, rows, row_width) in [
        (
            y,
            elf::STRIDE_Y,
            elf::DATA_Y,
            height as usize,
            width as usize,
        ),
        (
            u,
            elf::STRIDE_U,
            elf::DATA_U,
            height as usize / 2,
            width as usize / 2,
        ),
        (
            v,
            elf::STRIDE_V,
            elf::DATA_V,
            height as usize / 2,
            width as usize / 2,
        ),
    ] {
        let stride: BufferStride = unsafe { mem::transmute(addresses.get(stride_symbol)) };
        let data: BufferData = unsafe { mem::transmute(addresses.get(data_symbol)) };
        let stride = unsafe { stride(buffer) };
        let destination = unsafe { data(buffer) };
        if destination.is_null() || stride < row_width as i32 {
            unsafe { release_buffer(buffer) };
            return;
        }
        for row in 0..rows {
            unsafe {
                ptr::copy_nonoverlapping(
                    source.as_ptr().add(row * row_width),
                    destination.add(row * stride as usize),
                    row_width,
                );
            }
        }
    }

    let mut builder = Aligned::<0x100>([0; 0x100]);
    let mut frame = Aligned::<0x200>([0; 0x200]);
    let builder_create: unsafe extern "C" fn(*mut c_void) =
        unsafe { mem::transmute(addresses.get(elf::BUILDER_CREATE)) };
    let builder_buffer: unsafe extern "C" fn(*mut c_void, *const *mut c_void) -> *mut c_void =
        unsafe { mem::transmute(addresses.get(elf::BUILDER_BUFFER)) };
    let builder_timestamp: unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void =
        unsafe { mem::transmute(addresses.get(elf::BUILDER_TIMESTAMP)) };
    let builder_build: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        unsafe { mem::transmute(addresses.get(elf::BUILDER_BUILD)) };
    let builder_destroy: unsafe extern "C" fn(*mut c_void) =
        unsafe { mem::transmute(addresses.get(elf::BUILDER_DESTROY)) };
    let frame_destroy: unsafe extern "C" fn(*mut c_void) =
        unsafe { mem::transmute(addresses.get(elf::FRAME_DESTRUCTOR)) };
    unsafe {
        builder_create(builder.0.as_mut_ptr().cast());
        builder_buffer(builder.0.as_mut_ptr().cast(), &buffer);
        builder_timestamp(builder.0.as_mut_ptr().cast(), timestamp);
        builder_build(frame.0.as_mut_ptr().cast(), builder.0.as_mut_ptr().cast());
        builder_destroy(builder.0.as_mut_ptr().cast());
    }
    let callback_vtable = unsafe { *(callback as *const *const usize) };
    let decoded: unsafe extern "C" fn(*mut c_void, *mut c_void, i64, i32) -> i32 =
        unsafe { mem::transmute(*callback_vtable.add(4)) };
    unsafe {
        decoded(callback, frame.0.as_mut_ptr().cast(), 0, 0);
        frame_destroy(frame.0.as_mut_ptr().cast());
        release_buffer(buffer);
    }
}

unsafe fn release_buffer(buffer: *mut c_void) {
    let vtable = unsafe { *(buffer as *const *const usize) };
    let release: unsafe extern "C" fn(*mut c_void) = unsafe { mem::transmute(*vtable.add(1)) };
    unsafe { release(buffer) };
}

#[cfg(test)]
mod tests {
    use super::{
        Codec, Decoder, DecoderCallback, DecoderState, decoder_delete, nv12_to_i420, on_nvdec_frame,
    };
    use std::ptr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};

    #[test]
    fn callback_context_survives_decoder_delete() {
        let callback = Arc::new(DecoderCallback {
            callback: AtomicUsize::new(1),
        });
        let decoder = Box::into_raw(Box::new(Decoder {
            vtable: ptr::null(),
            codec: Codec::H264,
            state: Mutex::new(DecoderState { backend: None }),
            callback: Arc::clone(&callback),
        }));
        let in_flight = Arc::clone(&callback);
        drop(callback);
        let ready = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let worker_ready = Arc::clone(&ready);
        let worker_resume = Arc::clone(&resume);
        let worker = std::thread::spawn(move || {
            let context = Arc::as_ptr(&in_flight).cast_mut().cast();
            worker_ready.wait();
            worker_resume.wait();
            on_nvdec_frame(context, &[0; 6], 2, 2, 0);
            assert_eq!(in_flight.callback.load(Ordering::Acquire), 0);
        });
        ready.wait();
        unsafe { decoder_delete(decoder) };
        resume.wait();
        worker.join().unwrap();
    }

    #[test]
    fn converts_nv12_to_i420() {
        let input = [1, 2, 3, 4, 5, 6];
        let (y, u, v) = nv12_to_i420(&input, 2, 2).unwrap();
        assert_eq!(y, [1, 2, 3, 4]);
        assert_eq!(u, [5]);
        assert_eq!(v, [6]);
    }
}

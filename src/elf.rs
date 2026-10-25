use std::collections::HashMap;
use std::path::Path;

pub const DECODER_FACTORY: &str =
    "_ZN7discord5media19VideoDecoderFactory6CreateERKN6webrtc11EnvironmentERKNS2_14SdpVideoFormatE";
pub const ENCODER_FACTORY: &str = "_ZN7discord5media8encoding21DiscordEncoderFactory6CreateERKN6webrtc11EnvironmentERKNS3_14SdpVideoFormatE";
pub const ENCODER_ADD_HELPER: &str = "_ZZN7discord5media8encoding21DiscordEncoderFactory6CreateERKN6webrtc11EnvironmentERKNS3_14SdpVideoFormatEENK3$_0clIJbEEEDa11EncoderTypeDpOT_";
pub const HARDWARE_INFO: &str =
    "_ZNK7discord5media8electron20ElectronVideoDecoder14GetDecoderInfoEv";
pub const CODEC_TYPE: &str = "_ZN6webrtc24PayloadStringToCodecTypeERKNSt4__Cr12basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEE";
pub const BUFFER_CREATE: &str = "_ZN6webrtc10I420Buffer6CreateEii";
pub const DATA_Y: &str = "_ZN6webrtc10I420Buffer12MutableDataYEv";
pub const DATA_U: &str = "_ZN6webrtc10I420Buffer12MutableDataUEv";
pub const DATA_V: &str = "_ZN6webrtc10I420Buffer12MutableDataVEv";
pub const STRIDE_Y: &str = "_ZNK6webrtc10I420Buffer7StrideYEv";
pub const STRIDE_U: &str = "_ZNK6webrtc10I420Buffer7StrideUEv";
pub const STRIDE_V: &str = "_ZNK6webrtc10I420Buffer7StrideVEv";
pub const BUILDER_CREATE: &str = "_ZN6webrtc10VideoFrame7BuilderC1Ev";
pub const BUILDER_DESTROY: &str = "_ZN6webrtc10VideoFrame7BuilderD1Ev";
pub const BUILDER_BUILD: &str = "_ZN6webrtc10VideoFrame7Builder5buildEv";
pub const BUILDER_BUFFER: &str = "_ZN6webrtc10VideoFrame7Builder22set_video_frame_bufferERKNS_13scoped_refptrINS_16VideoFrameBufferEEE";
pub const BUILDER_TIMESTAMP: &str = "_ZN6webrtc10VideoFrame7Builder17set_timestamp_rtpEj";
pub const FRAME_DESTRUCTOR: &str = "_ZN6webrtc10VideoFrameD1Ev";

const SYMBOL_NAMES: [&str; 18] = [
    DECODER_FACTORY,
    ENCODER_FACTORY,
    ENCODER_ADD_HELPER,
    HARDWARE_INFO,
    CODEC_TYPE,
    BUFFER_CREATE,
    DATA_Y,
    DATA_U,
    DATA_V,
    STRIDE_Y,
    STRIDE_U,
    STRIDE_V,
    BUILDER_CREATE,
    BUILDER_DESTROY,
    BUILDER_BUILD,
    BUILDER_BUFFER,
    BUILDER_TIMESTAMP,
    FRAME_DESTRUCTOR,
];

#[derive(Clone, Copy)]
pub struct Symbol {
    pub offset: usize,
    pub size: usize,
}

pub struct Symbols(HashMap<&'static str, Symbol>);

impl Symbols {
    pub fn get(&self, name: &'static str) -> Option<Symbol> {
        self.0.get(name).copied()
    }
}

fn number<const SIZE: usize>(data: &[u8], offset: usize) -> Option<[u8; SIZE]> {
    data.get(offset..offset.checked_add(SIZE)?)?.try_into().ok()
}

fn short(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(number(data, offset)?))
}

fn word(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(number(data, offset)?))
}

fn long(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(number(data, offset)?))
}

fn section(data: &[u8], table: usize, entry_size: usize, index: usize) -> Option<&[u8]> {
    let offset = table.checked_add(index.checked_mul(entry_size)?)?;
    data.get(offset..offset.checked_add(entry_size)?)
}

fn content<'a>(data: &'a [u8], header: &[u8]) -> Option<&'a [u8]> {
    let offset = usize::try_from(long(header, 24)?).ok()?;
    let length = usize::try_from(long(header, 32)?).ok()?;
    data.get(offset..offset.checked_add(length)?)
}

pub fn read_symbols(path: &Path) -> Option<Symbols> {
    let data = std::fs::read(path).ok()?;
    if data.get(..6)? != b"\x7fELF\x02\x01" || short(&data, 16)? != 3 {
        return None;
    }
    let table = usize::try_from(long(&data, 40)?).ok()?;
    let entry_size = short(&data, 58)? as usize;
    let count = short(&data, 60)? as usize;
    if entry_size < 64 || count == 0 || count > 4096 {
        return None;
    }
    let mut found = HashMap::new();
    for index in 0..count {
        let header = section(&data, table, entry_size, index)?;
        if !matches!(word(header, 4)?, 2 | 11) {
            continue;
        }
        let symbols = content(&data, header)?;
        let string_index = word(header, 40)? as usize;
        let strings = content(&data, section(&data, table, entry_size, string_index)?)?;
        let symbol_size = usize::try_from(long(header, 56)?).ok()?;
        if symbol_size < 24 || symbols.len() / symbol_size > 1_000_000 {
            return None;
        }
        for entry in symbols.chunks_exact(symbol_size) {
            if entry[4] & 15 != 2 || short(entry, 6)? == 0 {
                continue;
            }
            let name_offset = word(entry, 0)? as usize;
            let Some(name_bytes) = strings.get(name_offset..) else {
                continue;
            };
            let Some(length) = name_bytes.iter().position(|byte| *byte == 0) else {
                continue;
            };
            let name = &name_bytes[..length];
            if let Some(required) = SYMBOL_NAMES
                .iter()
                .copied()
                .find(|required| required.as_bytes() == name)
            {
                found.insert(
                    required,
                    Symbol {
                        offset: usize::try_from(long(entry, 8)?).ok()?,
                        size: usize::try_from(long(entry, 16)?).ok()?,
                    },
                );
            }
        }
    }
    (found.len() == SYMBOL_NAMES.len()).then_some(Symbols(found))
}

//! 直接通过 dlopen 调用 libasound（运行时库），因此不需要 alsa 开发头文件。
//! 输出格式固定为 S16_LE 交错立体声，频率跟随当前曲目。

use libloading::Library;
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr, CString};
use std::ptr;

type SndPcmOpen = unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
type SndPcmSetParams =
    unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int;
type SndPcmWritei = unsafe extern "C" fn(*mut c_void, *const c_void, c_ulong) -> c_long;
type SndPcmSimple = unsafe extern "C" fn(*mut c_void) -> c_int;
type SndStrError = unsafe extern "C" fn(c_int) -> *const c_char;

const STREAM_PLAYBACK: c_int = 0;
const FORMAT_S16_LE: c_int = 2;
const ACCESS_RW_INTERLEAVED: c_int = 3;
const EAGAIN: c_int = -11;
const EPIPE: c_int = -32;
const ESTRPIPE: c_int = -86;

pub struct Pcm {
    pcm: *mut c_void,
    writei: SndPcmWritei,
    prepare: SndPcmSimple,
    drain: SndPcmSimple,
    drop_: SndPcmSimple,
    close: SndPcmSimple,
    strerror: SndStrError,
    pub rate: u32,
    pub channels: u16,
    /// 必须最后声明：保证函数指针在使用期间库仍被加载
    _lib: Library,
}

unsafe impl Send for Pcm {}

impl Pcm {
    /// 打开默认设备；失败时依次尝试 pipewire / pulse / plughw。
    pub fn open_default(rate: u32, channels: u16) -> Result<Self, String> {
        let mut last = String::from("没有可用的音频设备");
        for name in ["default", "pipewire", "pulse", "plughw:0,0"] {
            match Self::open(name, rate, channels) {
                Ok(p) => return Ok(p),
                Err(e) => last = format!("{name}: {e}"),
            }
        }
        Err(last)
    }

    pub fn open(device: &str, rate: u32, channels: u16) -> Result<Self, String> {
        unsafe {
            let lib = Library::new("libasound.so.2").map_err(|e| format!("加载 libasound 失败: {e}"))?;
            let sym = |name: &[u8]| -> Result<*mut c_void, String> {
                lib.get::<*mut c_void>(name)
                    .map(|s| *s)
                    .map_err(|e| format!("缺少符号 {}: {e}", String::from_utf8_lossy(name)))
            };
            let f_open: SndPcmOpen = std::mem::transmute(sym(b"snd_pcm_open\0")?);
            let f_set: SndPcmSetParams = std::mem::transmute(sym(b"snd_pcm_set_params\0")?);
            let writei: SndPcmWritei = std::mem::transmute(sym(b"snd_pcm_writei\0")?);
            let prepare: SndPcmSimple = std::mem::transmute(sym(b"snd_pcm_prepare\0")?);
            let drain: SndPcmSimple = std::mem::transmute(sym(b"snd_pcm_drain\0")?);
            let drop_: SndPcmSimple = std::mem::transmute(sym(b"snd_pcm_drop\0")?);
            let close: SndPcmSimple = std::mem::transmute(sym(b"snd_pcm_close\0")?);
            let strerror: SndStrError = std::mem::transmute(sym(b"snd_strerror\0")?);

            let cname = CString::new(device).map_err(|e| e.to_string())?;
            let mut pcm: *mut c_void = ptr::null_mut();
            let rc = f_open(&mut pcm, cname.as_ptr(), STREAM_PLAYBACK, 0);
            if rc < 0 {
                return Err(err_text(strerror, rc));
            }
            let rc = f_set(
                pcm,
                FORMAT_S16_LE,
                ACCESS_RW_INTERLEAVED,
                channels as c_uint,
                rate,
                1,
                120_000, // 120ms 缓冲，兼顾延迟与抗卡顿
            );
            if rc < 0 {
                close(pcm);
                return Err(err_text(strerror, rc));
            }
            Ok(Pcm {
                pcm,
                writei,
                prepare,
                drain,
                drop_,
                close,
                strerror,
                rate,
                channels,
                _lib: lib,
            })
        }
    }

    /// 写入交错 i16 采样，阻塞直到全部写完。
    pub fn write(&self, samples: &[i16]) -> Result<(), String> {
        if samples.is_empty() {
            return Ok(());
        }
        let ch = self.channels as usize;
        let frames = samples.len() / ch;
        let mut off = 0usize;
        let mut retried = 0;
        while off < frames {
            let ptr = unsafe { samples.as_ptr().add(off * ch) as *const c_void };
            let n = unsafe { (self.writei)(self.pcm, ptr, (frames - off) as c_ulong) };
            if n < 0 {
                let e = n as c_int;
                if e == EPIPE || e == ESTRPIPE || e == EAGAIN {
                    if retried > 4 {
                        return Err(err_text(self.strerror, e));
                    }
                    retried += 1;
                    unsafe {
                        (self.prepare)(self.pcm);
                    }
                    continue;
                }
                return Err(err_text(self.strerror, e));
            }
            retried = 0;
            off += n as usize;
        }
        Ok(())
    }

    pub fn flush(&self) {
        unsafe {
            (self.drop_)(self.pcm);
            (self.prepare)(self.pcm);
        }
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        unsafe {
            (self.drain)(self.pcm);
            (self.close)(self.pcm);
        }
    }
}

fn err_text(f: SndStrError, code: c_int) -> String {
    unsafe {
        let p = f(code);
        if p.is_null() {
            return format!("ALSA 错误 {code}");
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// 运行时能否加载 libasound（用于启动自检）
pub fn available() -> bool {
    unsafe { Library::new("libasound.so.2").is_ok() }
}

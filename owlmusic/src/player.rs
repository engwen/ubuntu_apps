//! 播放引擎：独立线程解码（symphonia）→ 音量 → ALSA 输出。
//! UI 通过 Shared 里的原子变量轮询位置与状态。

use crate::alsa::Pcm;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder as SymphoniaDecoder, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

pub const ST_STOPPED: u8 = 0;
pub const ST_PLAYING: u8 = 1;
pub const ST_PAUSED: u8 = 2;

const CHUNK_MS: u64 = 40;
/// 淡入淡出时长（毫秒）。起播、暂停、继续都走这段斜坡，
/// 避免「啪」的一下满音量出声 / 一刀切断。
const FADE_MS: f32 = 320.0;

pub struct Shared {
    pub pos_ms: AtomicU64,
    pub dur_ms: AtomicU64,
    pub state: AtomicU8,
    /// f32 位模式
    pub volume: AtomicU32,
    /// 每结束一首歌 +1，UI 靠它切下一首
    pub finished: AtomicU64,
    pub error: Mutex<Option<String>>,
}

impl Shared {
    pub fn state(&self) -> u8 {
        self.state.load(Ordering::Relaxed)
    }
    pub fn pos(&self) -> u64 {
        self.pos_ms.load(Ordering::Relaxed)
    }
    pub fn dur(&self) -> u64 {
        self.dur_ms.load(Ordering::Relaxed)
    }
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut g| g.take())
    }
}

enum Cmd {
    Load {
        path: PathBuf,
        seq: u64,
        dur_ms: u64,
        autoplay: bool,
    },
    Play,
    Pause,
    Seek(u64),
    Stop,
}

pub struct Player {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    seq: AtomicU64,
}

impl Player {
    pub fn new(volume: f32) -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            pos_ms: AtomicU64::new(0),
            dur_ms: AtomicU64::new(0),
            state: AtomicU8::new(ST_STOPPED),
            volume: AtomicU32::new(f32::to_bits(volume)),
            finished: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let sh = shared.clone();
        thread::Builder::new()
            .name("owlmusic-engine".into())
            .spawn(move || engine(rx, sh))
            .expect("spawn engine");
        Player {
            tx,
            shared,
            seq: AtomicU64::new(1),
        }
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    pub fn load(&self, path: PathBuf, dur_ms: u64, autoplay: bool) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.shared.pos_ms.store(0, Ordering::Relaxed);
        self.shared.dur_ms.store(dur_ms, Ordering::Relaxed);
        self.shared.state.store(
            if autoplay { ST_PLAYING } else { ST_PAUSED },
            Ordering::Relaxed,
        );
        let _ = self.tx.send(Cmd::Load {
            path,
            seq,
            dur_ms,
            autoplay,
        });
        seq
    }

    pub fn play(&self) {
        if self.shared.state() == ST_PAUSED {
            self.shared.state.store(ST_PLAYING, Ordering::Relaxed);
        }
        let _ = self.tx.send(Cmd::Play);
    }

    pub fn pause(&self) {
        if self.shared.state() == ST_PLAYING {
            self.shared.state.store(ST_PAUSED, Ordering::Relaxed);
        }
        let _ = self.tx.send(Cmd::Pause);
    }

    pub fn stop(&self) {
        self.shared.state.store(ST_STOPPED, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn seek(&self, ms: u64) {
        self.shared.pos_ms.store(ms, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Seek(ms));
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.set_volume(v);
    }
}

fn engine(rx: Receiver<Cmd>, shared: Arc<Shared>) {
    let mut pcm: Option<Pcm> = None;
    let mut dec: Option<Decoder> = None;
    let mut base_ms = 0u64;
    let mut frames = 0u64;
    let mut rate = 44_100u32;
    let mut channels = 2u16;
    let mut out: Vec<f32> = Vec::with_capacity(16_384);
    // 当前淡变增益 0..1：起播从 0 升到 1，暂停从 1 降到 0
    let mut fade: f32 = 0.0;

    loop {
        // 命令处理
        let mut keep_going = true;
        while keep_going {
            match rx.try_recv() {
                Ok(Cmd::Load {
                    path,
                    seq: _,
                    dur_ms,
                    autoplay,
                }) => {
                    match Decoder::open(&path) {
                        Ok(d) => {
                            rate = d.rate;
                            channels = d.channels;
                            let need = match &pcm {
                                Some(p) => p.rate != rate || p.channels != channels,
                                None => true,
                            };
                            if need {
                                pcm = None;
                                match Pcm::open_default(rate, channels) {
                                    Ok(p) => pcm = Some(p),
                                    Err(e) => {
                                        if let Ok(mut g) = shared.error.lock() {
                                            *g = Some(e);
                                        }
                                        shared.state.store(ST_STOPPED, Ordering::Relaxed);
                                    }
                                }
                            } else if let Some(p) = &pcm {
                                p.flush();
                            }
                            base_ms = 0;
                            frames = 0;
                            shared.pos_ms.store(0, Ordering::Relaxed);
                            shared.dur_ms.store(dur_ms, Ordering::Relaxed);
                            dec = Some(d);
                            shared
                                .state
                                .store(if autoplay { ST_PLAYING } else { ST_PAUSED }, Ordering::Relaxed);
                        }
                        Err(e) => {
                            if let Ok(mut g) = shared.error.lock() {
                                *g = Some(e);
                            }
                            shared.state.store(ST_STOPPED, Ordering::Relaxed);
                            dec = None;
                        }
                    }
                    // 换曲一律从静音开始淡入，哪怕文件打不开（下一首也一样）
                    fade = 0.0;
                }
                Ok(Cmd::Play) => {
                    shared.state.store(ST_PLAYING, Ordering::Relaxed);
                }
                Ok(Cmd::Pause) => {
                    // 只改状态：引擎会在接下来的几块里把音量降到 0 再停，
                    // 所以这里不能 flush，否则就成了硬切
                    shared.state.store(ST_PAUSED, Ordering::Relaxed);
                }
                Ok(Cmd::Stop) => {
                    dec = None;
                    fade = 0.0;
                    shared.state.store(ST_STOPPED, Ordering::Relaxed);
                    shared.pos_ms.store(0, Ordering::Relaxed);
                    if let Some(p) = &pcm {
                        p.flush();
                    }
                }
                Ok(Cmd::Seek(ms)) => {
                    if let Some(d) = dec.as_mut() {
                        match d.seek_ms(ms) {
                            Ok(()) => {
                                base_ms = ms;
                                frames = 0;
                                shared.pos_ms.store(ms, Ordering::Relaxed);
                                if let Some(p) = &pcm {
                                    p.flush();
                                }
                            }
                            Err(e) => {
                                if let Ok(mut g) = shared.error.lock() {
                                    *g = Some(e);
                                }
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => keep_going = false,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let state = shared.state();

        if dec.is_none() {
            fade = 0.0;
            thread::sleep(Duration::from_millis(12));
            continue;
        }

        // 播放态目标满音量，暂停/停止目标静音。
        // 关键点：淡出期间还要继续解码并写出逐渐变小的声音，
        // 直接停下来的话，ALSA 里已经缓冲的那一块会被硬切。
        let target = if state == ST_PLAYING { 1.0 } else { 0.0 };
        if state != ST_PLAYING && fade <= 0.0005 {
            fade = 0.0;
            thread::sleep(Duration::from_millis(12));
            continue;
        }

        // 解码一块并播放
        let chunk = {
            let d = dec.as_mut().unwrap();
            match d.read_into(&mut out) {
                Ok(true) => Some(()),
                Ok(false) => None,
                Err(e) => {
                    if let Ok(mut g) = shared.error.lock() {
                        *g = Some(e);
                    }
                    None
                }
            }
        };

        match chunk {
            Some(()) => {
                let vol = shared.volume();
                let gain = vol * vol; // 平方律，接近听感线性
                let ch = channels as usize;
                let played_frames = (out.len() / ch.max(1)) as u64;

                // 按这一块的真实时长推进增益：320ms 的淡变在 40ms 一块时约 8 块走完
                let chunk_ms = played_frames as f32 * 1000.0 / rate.max(1) as f32;
                let next = fade_next(fade, target, chunk_ms);

                // 块内做线性插值，块与块之间才不会出现台阶状的音量跳变
                let n = out.len().max(1) as f32;
                let mut buf: Vec<i16> = Vec::with_capacity(out.len());
                for (i, s) in out.iter().enumerate() {
                    let g = fade + (next - fade) * (i as f32 / n);
                    let v = (s * gain * g * 32767.0).clamp(-32768.0, 32767.0);
                    buf.push(v as i16);
                }
                fade = next;

                if let Some(p) = &pcm {
                    if let Err(e) = p.write(&buf) {
                        if let Ok(mut g) = shared.error.lock() {
                            *g = Some(e);
                        }
                        shared.state.store(ST_STOPPED, Ordering::Relaxed);
                        continue;
                    }
                } else {
                    // 没有声卡也按真实速度推进，便于无设备环境调试
                    thread::sleep(Duration::from_millis(CHUNK_MS));
                }
                frames += played_frames;
                let pos = base_ms + frames * 1000 / rate.max(1) as u64;
                shared.pos_ms.store(pos, Ordering::Relaxed);
            }
            None => {
                if dec.is_some() {
                    dec = None;
                    fade = 0.0;
                    shared.state.store(ST_STOPPED, Ordering::Relaxed);
                    let done = shared.pos_ms.load(Ordering::Relaxed);
                    let total = shared.dur_ms.load(Ordering::Relaxed);
                    let at_end = total == 0 || done + 800 >= total;
                    if at_end {
                        shared.pos_ms.store(total, Ordering::Relaxed);
                    }
                    shared.finished.fetch_add(1, Ordering::Relaxed);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn SymphoniaDecoder>,
    track_id: u32,
    rate: u32,
    channels: u16,
    buf: Option<SampleBuffer<f32>>,
    eof: bool,
}

impl Decoder {
    fn open(path: &PathBuf) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("打开文件失败: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| format!("无法识别音频格式: {e}"))?;
        let format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| "文件里没有可播放的音轨".to_string())?;
        let track_id = track.id;
        let params = track.codec_params.clone();
        let rate = params.sample_rate.unwrap_or(44_100);
        let channels = params
            .channels
            .map(|c| c.count() as u16)
            .unwrap_or(2);
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| format!("不支持的编码: {e}"))?;
        Ok(Decoder {
            format,
            decoder,
            track_id,
            rate,
            channels,
            buf: None,
            eof: false,
        })
    }

    fn seek_ms(&mut self, ms: u64) -> Result<(), String> {
        let t = Time {
            seconds: ms / 1000,
            frac: (ms % 1000) as f64 / 1000.0,
        };
        self.format
            .seek(SeekMode::Accurate, SeekTo::Time { time: t, track_id: Some(self.track_id) })
            .map_err(|e| format!("跳转失败: {e}"))?;
        self.decoder.reset();
        self.eof = false;
        Ok(())
    }

    /// 读取下一块交错 f32 采样；返回 false 表示播放结束。
    fn read_into(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        out.clear();
        while !self.eof {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    return Ok(false);
                }
                Err(SymError::ResetRequired) => {
                    self.eof = true;
                    return Ok(false);
                }
                Err(e) => return Err(format!("读取失败: {e}")),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    let spec = *audio.spec();
                    let cap = audio.capacity();
                    let mismatch = self.rate != spec.rate
                        || self.channels != spec.channels.count() as u16;
                    let need_new = match &self.buf {
                        Some(b) => b.capacity() < cap || mismatch,
                        None => true,
                    };
                    if need_new {
                        self.buf = Some(SampleBuffer::<f32>::new(cap as u64, spec));
                    }
                    if let Some(buf) = self.buf.as_mut() {
                        buf.copy_interleaved_ref(audio);
                        out.extend_from_slice(buf.samples());
                    }
                    self.rate = spec.rate;
                    self.channels = spec.channels.count() as u16;
                    if out.is_empty() {
                        continue;
                    }
                    return Ok(true);
                }
                Err(SymError::DecodeError(_)) => continue,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    return Ok(false);
                }
                Err(e) => return Err(format!("解码失败: {e}")),
            }
        }
        Ok(false)
    }
}

/// 诊断用：打开文件、真的解一段出来，告诉用户是「容器认不出」还是「编码不支持」。
/// `owlmusic --probe 某个文件` 会调它，排查「这首为什么播不了」时比看日志快。
pub fn probe_file(path: &std::path::Path) -> Result<String, String> {
    let mut d = Decoder::open(&path.to_path_buf())?;
    let mut out = Vec::new();
    let mut frames = 0usize;
    let mut samples = 0usize;
    for _ in 0..40 {
        match d.read_into(&mut out)? {
            true => {
                frames += 1;
                samples += out.len();
            }
            false => break,
        }
    }
    if frames == 0 {
        return Err("解不出任何音频数据".into());
    }
    let ms = samples as f64 / (d.rate as f64 * d.channels as f64) * 1000.0;
    Ok(format!(
        "{} Hz · {} 声道 · 共解出 {} 帧 / {:.0} ms 音频",
        d.rate, d.channels, frames, ms
    ))
}


/// 淡变增益的下一步：按这一块的真实时长朝目标走一格。
///
/// 抽成纯函数是为了能单测——引擎里那段真实时间是 40ms 一块，
/// 想验证「320ms 内到满音量」「不会过冲」只能靠算，不能靠听。
fn fade_next(cur: f32, target: f32, chunk_ms: f32) -> f32 {
    let step = (chunk_ms / FADE_MS).clamp(0.0, 1.0);
    if cur < target {
        (cur + step).min(target)
    } else {
        (cur - step).max(target)
    }
}

/// 预留给以后做播放队列的环形缓冲
#[allow(dead_code)]
struct Ring {
    data: VecDeque<f32>,
}

/// 兼容不同 TB 类型的工具
#[allow(dead_code)]
fn frames_to_ms(frames: u64, tb: TimeBase) -> u64 {
    let t = tb.calc_time(frames);
    t.seconds * 1000 + (t.frac * 1000.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起播必须从静音爬满，而且 320ms 内正好到 1.0：不能一出声就是满音量。
    #[test]
    fn fade_in_reaches_full_in_one_fade_window() {
        let mut g = 0.0f32;
        let mut ms = 0.0f32;
        while g < 1.0 && ms < 2000.0 {
            let next = fade_next(g, 1.0, CHUNK_MS as f32);
            assert!(next >= g, "淡入不能回头");
            g = next;
            ms += CHUNK_MS as f32;
        }
        assert_eq!(g, 1.0);
        assert!(ms <= FADE_MS + CHUNK_MS as f32, "淡入用时应约等于 FADE_MS，实际 {ms}ms");
    }

    /// 暂停时同理：降到 0 就停住，不会负数、也不会过冲。
    #[test]
    fn fade_out_settles_at_zero() {
        let mut g = 1.0f32;
        for _ in 0..40 {
            g = fade_next(g, 0.0, CHUNK_MS as f32);
        }
        assert_eq!(g, 0.0);
    }

    /// 暂停到一半又点了播放：增益要从当前位置往回爬，不能跳变也不越过 1。
    #[test]
    fn fade_reverses_midway() {
        let mut g = 0.0f32;
        for _ in 0..4 {
            g = fade_next(g, 1.0, CHUNK_MS as f32);
        }
        let mid = g;
        assert!(mid > 0.0 && mid < 1.0);
        g = fade_next(g, 0.0, CHUNK_MS as f32);
        assert!(g < mid && g >= 0.0);
        g = fade_next(g, 1.0, CHUNK_MS as f32);
        assert!(g > 0.0 && g <= 1.0);
    }
}

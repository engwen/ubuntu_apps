//! MPRIS 服务：把正在播放的内容暴露给桌面。
//!
//! GNOME 的媒体控制中心、Media Controls / Top Bar Lyrics 这类插件、键盘多媒体键、
//! `playerctl` 走的都是这条总线。这里直接用 GDBus（gio）注册对象，不引 zbus / tokio，
//! 也就不会把 tokio 运行时带进 GTK 主循环。
//!
//! 两个接口挂在同一个对象路径下：
//!   * `org.mpris.MediaPlayer2`        —— 应用级（Raise / Quit / Identity …）
//!   * `org.mpris.MediaPlayer2.Player` —— 播放级（播放控制 / 元数据 / 进度 / 音量）
//!
//! 外部按下的按钮通过 [`Control`] 回到 UI 里的真实动作；UI 每帧调用 [`Mpris::sync`]，
//! 播放状态变化时只广播一次 `PropertiesChanged`，不会每 33ms 刷一遍总线。

use crate::player::{Shared, ST_PAUSED, ST_PLAYING};
use gtk::gio::{self, prelude::*};
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.owlmusic";
const OBJ_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const DESKTOP_ENTRY: &str = crate::desktop::APP_ID;
const TRACK_PATH: &str = "/org/owlmusic/track";

/// 桌面能反过来控制我们的那几件事。UI 在窗口建好后注入。
pub struct Control {
    pub play_pause: Box<dyn Fn()>,
    pub next: Box<dyn Fn()>,
    pub prev: Box<dyn Fn()>,
    pub seek_ms: Box<dyn Fn(u64)>,
    pub set_volume: Box<dyn Fn(f64)>,
    pub raise: Box<dyn Fn()>,
    pub quit: Box<dyn Fn()>,
}

const XML: &str = r#"<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek">
      <arg name="Offset" type="x" direction="in"/>
    </method>
    <method name="SetPosition">
      <arg name="TrackId" type="o" direction="in"/>
      <arg name="Position" type="x" direction="in"/>
    </method>
    <method name="OpenUri">
      <arg name="Uri" type="s" direction="in"/>
    </method>
    <signal name="Seeked">
      <arg name="Position" type="x"/>
    </signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="read"/>
    <property name="Rate" type="d" access="read"/>
    <property name="Shuffle" type="b" access="read"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>"#;

struct Meta {
    title: String,
    artist: String,
    album: String,
    art_url: Option<String>,
    /// 音频文件自己的 file:// URL（GNOME 用它「在文件夹中显示」）
    file_url: Option<String>,
    /// MPRIS 的曲目身份。每换一首递增，客户端靠它判断「换歌了」。
    track_id: String,
    dur_ms: u64,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            title: "未知曲目".into(),
            artist: "未知艺人".into(),
            album: "未知专辑".into(),
            art_url: None,
            file_url: None,
            track_id: format!("{TRACK_PATH}/0"),
            dur_ms: 0,
        }
    }
}

struct Inner {
    shared: Arc<Shared>,
    conn: gio::DBusConnection,
    control: RefCell<Option<Control>>,
    meta: RefCell<Meta>,
    shuffle: Cell<bool>,
    /// 0 关 1 列表循环 2 单曲循环
    repeat: Cell<u8>,
    /// 上一次已广播 / 已设置过的音量，避免外部设音量时打回声
    volume: Cell<f64>,
    /// 上一次广播出去的播放状态，用来检测变化
    status: Cell<u8>,
    /// 曲目序号：每次换歌 +1，拼进 trackid
    track_seq: Cell<u64>,
}

impl Inner {
    fn playback_status(&self) -> &'static str {
        match self.shared.state() {
            ST_PLAYING => "Playing",
            ST_PAUSED => "Paused",
            _ => "Stopped",
        }
    }

    fn loop_status(&self) -> &'static str {
        match self.repeat.get() {
            1 => "Playlist",
            2 => "Track",
            _ => "None",
        }
    }

    fn metadata(&self) -> glib::Variant {
        let m = self.meta.borrow();
        let d = glib::VariantDict::new(None);
        if let Ok(path) = glib::variant::ObjectPath::try_from(m.track_id.as_str()) {
            d.insert("mpris:trackid", path);
        }
        d.insert("mpris:length", m.dur_ms as i64 * 1000);
        d.insert("xesam:title", m.title.clone());
        d.insert("xesam:artist", vec![m.artist.clone()]);
        d.insert("xesam:album", m.album.clone());
        if let Some(u) = &m.file_url {
            d.insert("xesam:url", u.clone());
        }
        if let Some(a) = &m.art_url {
            d.insert("mpris:artUrl", a.clone());
        }
        d.end()
    }

    fn get_prop(&self, iface: &str, prop: &str) -> glib::Variant {
        match (iface, prop) {
            (ROOT_IFACE, "CanQuit") | (ROOT_IFACE, "CanRaise") => true.to_variant(),
            (ROOT_IFACE, "HasTrackList") => false.to_variant(),
            (ROOT_IFACE, "Identity") => "OwlMusic".to_variant(),
            (ROOT_IFACE, "DesktopEntry") => DESKTOP_ENTRY.to_variant(),
            (ROOT_IFACE, "SupportedUriSchemes") => vec!["file"].to_variant(),
            (ROOT_IFACE, "SupportedMimeTypes") => vec![
                "audio/mpeg",
                "audio/flac",
                "audio/mp4",
                "audio/ogg",
                "audio/x-vorbis+ogg",
                "audio/wav",
                "audio/x-wav",
            ]
            .to_variant(),

            (PLAYER_IFACE, "PlaybackStatus") => self.playback_status().to_variant(),
            (PLAYER_IFACE, "LoopStatus") => self.loop_status().to_variant(),
            (PLAYER_IFACE, "Rate")
            | (PLAYER_IFACE, "MinimumRate")
            | (PLAYER_IFACE, "MaximumRate") => 1.0f64.to_variant(),
            (PLAYER_IFACE, "Shuffle") => self.shuffle.get().to_variant(),
            (PLAYER_IFACE, "Metadata") => self.metadata(),
            (PLAYER_IFACE, "Volume") => self.volume.get().to_variant(),
            (PLAYER_IFACE, "Position") => (self.shared.pos() as i64 * 1000).to_variant(),
            (PLAYER_IFACE, "CanGoNext")
            | (PLAYER_IFACE, "CanGoPrevious")
            | (PLAYER_IFACE, "CanPlay")
            | (PLAYER_IFACE, "CanPause")
            | (PLAYER_IFACE, "CanSeek")
            | (PLAYER_IFACE, "CanControl") => true.to_variant(),

            // 接口里没声明的属性：给一个空字符串，总比让调用方拿到错误强
            _ => "".to_variant(),
        }
    }

    fn set_prop(&self, iface: &str, prop: &str, value: &glib::Variant) -> bool {
        if iface != PLAYER_IFACE || prop != "Volume" {
            return false;
        }
        let Some(v) = value.get::<f64>() else {
            return false;
        };
        let v = v.clamp(0.0, 1.0);
        let changed = (v - self.volume.get()).abs() > 1e-4;
        self.volume.set(v);
        // 先推给引擎，界面上的音量条再由 UI 自己同步
        self.shared.set_volume(v as f32);
        if let Some(c) = self.control.borrow().as_ref() {
            (c.set_volume)(v);
        }
        if changed {
            self.emit_one("Volume", v.to_variant());
        }
        true
    }

    fn call(&self, iface: &str, method: &str, params: &glib::Variant) {
        let ctrl = self.control.borrow();
        let Some(c) = ctrl.as_ref() else { return };
        match (iface, method) {
            (ROOT_IFACE, "Raise") => (c.raise)(),
            (ROOT_IFACE, "Quit") => (c.quit)(),
            (PLAYER_IFACE, "Next") => (c.next)(),
            (PLAYER_IFACE, "Previous") => (c.prev)(),
            (PLAYER_IFACE, "PlayPause") => (c.play_pause)(),
            // Play / Pause 只有在当前状态确实相反时才动，避免「按播放却暂停」
            (PLAYER_IFACE, "Play") => {
                if self.shared.state() != ST_PLAYING {
                    (c.play_pause)();
                }
            }
            (PLAYER_IFACE, "Pause") => {
                if self.shared.state() == ST_PLAYING {
                    (c.play_pause)();
                }
            }
            (PLAYER_IFACE, "Stop") => {
                if self.shared.state() == ST_PLAYING {
                    (c.play_pause)();
                }
                (c.seek_ms)(0);
            }
            (PLAYER_IFACE, "Seek") => {
                let offset = params.get::<i64>().unwrap_or(0);
                let cur = self.shared.pos() as i64 * 1000;
                let dur = self.shared.dur() as i64 * 1000;
                let target = (cur + offset).clamp(0, dur.max(0));
                self.seek_to(target, c);
            }
            (PLAYER_IFACE, "SetPosition") => {
                let Some((track, pos)) = params.get::<(glib::variant::ObjectPath, i64)>() else {
                    return;
                };
                // 只有当前曲目的 trackid 才对得上；客户端拿着旧 id 来的请求直接忽略
                if track.as_str() != self.meta.borrow().track_id {
                    return;
                }
                let dur = self.shared.dur() as i64 * 1000;
                self.seek_to(pos.clamp(0, dur.max(0)), c);
            }
            // 打开外部 URI 不支持：接受请求但不做事，免得客户端收到错误后反复重试
            (PLAYER_IFACE, "OpenUri") => {}
            _ => {}
        }
    }

    fn seek_to(&self, pos_us: i64, c: &Control) {
        let ms = (pos_us.max(0) as u64) / 1000;
        (c.seek_ms)(ms);
        // 让客户端把进度条立刻挪过去，不用等下一次轮询
        let _ = self.conn.emit_signal(
            None::<&str>,
            OBJ_PATH,
            PLAYER_IFACE,
            "Seeked",
            Some(&(pos_us.max(0)).to_variant()),
        );
    }

    fn emit_one(&self, prop: &str, value: glib::Variant) {
        let mut changed = HashMap::new();
        changed.insert(prop.to_string(), value);
        self.emit(changed);
    }

    /// 广播 `org.freedesktop.DBus.Properties.PropertiesChanged`。
    ///
    /// 参数签名必须严格是 `(sa{sv}as)`。这里**不能**把字典当成一个 `glib::Variant`
    /// 塞进元组：元组各元素走 `ToVariant` 时，`Variant` 会被包成单个 `v`，
    /// 实际发出去就成了 `(svas)`。GLib 客户端会以
    /// 「Value for PropertiesChanged signal with type '(svas)' does not match
    /// '(sa{sv}as)'」丢弃整条信号——属性 Get 照常能读，只有推送全丢，
    /// 于是桌面面板永远停在上一次的状态。
    /// 用 `HashMap<String, Variant>` 才会被编码成真正的 `a{sv}`。
    fn emit(&self, changed: HashMap<String, glib::Variant>) {
        let params = properties_changed_params(changed);
        if params.type_().as_str() != "(sa{sv}as)" {
            crate::guard::log(&format!(
                "MPRIS: PropertiesChanged 签名异常 {}（应为 (sa{{sv}}as)），已丢弃",
                params.type_()
            ));
            return;
        }
        if let Err(e) = self.conn.emit_signal(
            None::<&str>,
            OBJ_PATH,
            PROPS_IFACE,
            "PropertiesChanged",
            Some(&params),
        ) {
            crate::guard::log(&format!("MPRIS: 广播属性变化失败：{e}"));
        }
    }
}

/// 组装 `PropertiesChanged` 的入参 `(sa{sv}as)`。
///
/// 单独抽出来是为了能被单元测试直接验证签名——这条签名一旦错成 `(svas)`，
/// GLib 客户端会静默丢弃信号，而 `gdbus monitor` 之类的工具照样能打印出来，
/// 光看总线流量是发现不了的。
fn properties_changed_params(changed: HashMap<String, glib::Variant>) -> glib::Variant {
    (PLAYER_IFACE, changed, Vec::<String>::new()).to_variant()
}

/// 对外句柄。字段是 Rc 的内部状态，克隆的是句柄本身。
pub struct Mpris {
    inner: Rc<Inner>,
}

impl Mpris {
    /// 注册 MPRIS 服务。连不上 session bus / 对象注册失败时返回 None。
    pub fn start(shared: Arc<Shared>, volume: f64) -> Option<Rc<Mpris>> {
        let conn = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(c) => c,
            Err(e) => {
                crate::guard::log(&format!("MPRIS: 连不上 session bus：{e}"));
                return None;
            }
        };
        let node = gio::DBusNodeInfo::for_xml(XML).ok()?;
        let root = node.lookup_interface(ROOT_IFACE)?;
        let player = node.lookup_interface(PLAYER_IFACE)?;

        let inner = Rc::new(Inner {
            shared,
            conn: conn.clone(),
            control: RefCell::new(None),
            meta: RefCell::new(Meta::default()),
            shuffle: Cell::new(false),
            repeat: Cell::new(0),
            volume: Cell::new(volume.clamp(0.0, 1.0)),
            status: Cell::new(u8::MAX),
            track_seq: Cell::new(0),
        });

        {
            let im = inner.clone();
            let ip = inner.clone();
            if conn
                .register_object(OBJ_PATH, &root)
                .method_call(move |_, _, _, iface, method, params, inv| {
                    // 先执行动作再回回复：客户端拿到返回时 UI 已经动了
                    im.call(iface.unwrap_or(""), method, &params);
                    inv.return_value(None::<&glib::Variant>);
                })
                .property(move |_, _, _, iface, prop| ip.get_prop(iface, prop))
                .build()
                .is_err()
            {
                crate::guard::log("MPRIS: 注册 MediaPlayer2 接口失败");
                return None;
            }
        }
        {
            let im = inner.clone();
            let ip = inner.clone();
            let is = inner.clone();
            if conn
                .register_object(OBJ_PATH, &player)
                .method_call(move |_, _, _, iface, method, params, inv| {
                    im.call(iface.unwrap_or(""), method, &params);
                    inv.return_value(None::<&glib::Variant>);
                })
                .property(move |_, _, _, iface, prop| ip.get_prop(iface, prop))
                .set_property(move |_, _, _, iface, prop, value| is.set_prop(iface, prop, &value))
                .build()
                .is_err()
            {
                crate::guard::log("MPRIS: 注册 Player 接口失败");
                return None;
            }
        }

        gio::bus_own_name_on_connection(
            &conn,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            |_, _| crate::guard::log(&format!("MPRIS: 服务已注册 {OBJ_PATH}")),
            |_, name| crate::guard::log(&format!("MPRIS: 已占用 {name}")),
        );

        Some(Rc::new(Mpris { inner }))
    }

    /// UI 注入反向控制。
    pub fn set_control(&self, c: Control) {
        *self.inner.control.borrow_mut() = Some(c);
    }

    /// 换歌：更新元数据并广播。
    pub fn set_track(
        &self,
        title: &str,
        artist: &str,
        album: &str,
        dur_ms: u64,
        path: &Path,
        art: Option<std::path::PathBuf>,
    ) {
        let seq = self.inner.track_seq.get().wrapping_add(1);
        self.inner.track_seq.set(seq);
        {
            let mut m = self.inner.meta.borrow_mut();
            m.title = if title.trim().is_empty() {
                "未知曲目".into()
            } else {
                title.to_string()
            };
            m.artist = if artist.trim().is_empty() {
                "未知艺人".into()
            } else {
                artist.to_string()
            };
            m.album = if album.trim().is_empty() {
                "未知专辑".into()
            } else {
                album.to_string()
            };
            m.dur_ms = dur_ms;
            m.file_url = Some(file_url(path));
            // 每首歌一个不同的 trackid：客户端据此判断「换歌了」。
            // 保持不变的话，GNOME 面板可能把新歌当成同一首而不刷新。
            m.track_id = format!("{TRACK_PATH}/{seq}");
            // 没有内嵌封面就不给 artUrl，别让桌面去加载一个空地址
            m.art_url = art.map(|p| file_url(&p));
        }
        self.inner.emit_one("Metadata", self.inner.metadata());
    }

    /// 当前曲目被移出列表 / 停止播放：清掉桌面上的元数据。
    pub fn clear(&self) {
        *self.inner.meta.borrow_mut() = Meta::default();
        self.inner.emit_one("Metadata", self.inner.metadata());
    }

    pub fn set_shuffle(&self, on: bool) {
        if self.inner.shuffle.replace(on) != on {
            self.inner.emit_one("Shuffle", on.to_variant());
        }
    }

    /// 0 关 1 列表循环 2 单曲循环
    pub fn set_repeat(&self, mode: u8) {
        let mode = mode.min(2);
        if self.inner.repeat.replace(mode) != mode {
            self.inner
                .emit_one("LoopStatus", self.inner.loop_status().to_variant());
        }
    }

    pub fn set_volume(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        // 界面自己拖音量条时 shared 已经更新过，这里只同步缓存并广播
        let changed = (v - self.inner.volume.get()).abs() > 1e-4;
        self.inner.volume.set(v);
        if changed {
            self.inner.emit_one("Volume", v.to_variant());
        }
    }

    /// 每帧调用：只在播放状态真的变化时才广播。
    pub fn sync(&self) {
        let st = self.inner.shared.state();
        if st != self.inner.status.get() {
            self.inner.status.set(st);
            self.inner
                .emit_one("PlaybackStatus", self.inner.playback_status().to_variant());
        }
    }
}

/// 本地文件 → `file://` URL，非保留字符按百分号编码。
/// GNOME 的媒体卡片拿 artUrl 去加载，路径里有空格 / 中文时不编码会加载失败。
fn file_url(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 8);
    out.push_str("file://");
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_declares_both_interfaces() {
        let node = gio::DBusNodeInfo::for_xml(XML).expect("XML 必须能被 GDBus 解析");
        assert!(node.lookup_interface(ROOT_IFACE).is_some());
        assert!(node.lookup_interface(PLAYER_IFACE).is_some());
    }

    #[test]
    fn file_url_encodes_spaces_and_non_ascii() {
        assert_eq!(file_url(Path::new("/tmp/a b.mp3")), "file:///tmp/a%20b.mp3");
        let u = file_url(Path::new("/tmp/晴天.mp3"));
        assert!(u.starts_with("file:///tmp/"));
        assert!(u.ends_with(".mp3"));
        assert!(!u.contains('晴'));
    }

    #[test]
    fn player_interface_exposes_metadata_and_volume() {
        let node = gio::DBusNodeInfo::for_xml(XML).expect("XML 必须能被 GDBus 解析");
        let player = node.lookup_interface(PLAYER_IFACE).unwrap();
        assert!(player.lookup_property("Metadata").is_some());
        assert!(player.lookup_property("Volume").is_some());
        assert!(player.lookup_method("SetPosition").is_some());
    }

    /// 回归：PropertiesChanged 的入参签名必须是 `(sa{sv}as)`。
    /// 曾经把字典当成一个 `Variant` 塞进元组，发出去变成 `(svas)`，
    /// GLib 客户端直接丢弃，桌面面板从此收不到任何状态推送。
    #[test]
    fn properties_changed_params_has_standard_signature() {
        let mut changed = HashMap::new();
        changed.insert(
            "PlaybackStatus".to_string(),
            "Playing".to_variant(),
        );
        let params = properties_changed_params(changed);
        assert_eq!(params.type_().as_str(), "(sa{sv}as)");

        // 空字典也不能退化
        assert_eq!(
            properties_changed_params(HashMap::new()).type_().as_str(),
            "(sa{sv}as)"
        );
    }
}

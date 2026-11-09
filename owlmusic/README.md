# OwlMusic

一个用 **Rust + GTK4** 写的本地音乐播放器，按 Ubuntu / GNOME 的视觉语言做：顶部标题栏、左侧歌单、右侧封面 + 同步歌词、底部常驻播放条。不联网也能用，联网只是用来补标签和歌词。

```
┌──────────────────────────────────────────────────────────┐
│  OwlMusic                                     ＋ ↻ ⚙ — □ × │  ← 固定 54px
├──────────────────┬───────────────────────────────────────┤
│  歌单（可搜索）   │   封面 + 曲目信息                      │  ← 弹性
│  ┌────────────┐  │   歌词（背景 = 毛玻璃化的专辑封面）    │
│  │ 01 曲名     │  │   当前行高亮、平滑滚动                 │
│  └────────────┘  │                                       │
├──────────────────┴───────────────────────────────────────┤
│  ⏮  ▶  ⏭   0:42 ──────●──────── 3:58   🔊 ────  ⤢        │  ← 固定 76px
└──────────────────────────────────────────────────────────┘
```

顶栏和播放条是**外壳**，高度写死（`ui::HEADER_H` / `ui::BAR_H`），只有中间的歌单和歌词区随窗口伸缩。
封面用的是按显示尺寸生成的缩略纹理（`art::square`）——`gtk::Picture` 的布局尺寸跟着纹理走，
直接塞原图会让一张 1000px 的封面把播放条顶高。

## 功能

**播放**
- 格式：MP3 / FLAC / M4A(ALAC/AAC) / OGG / WAV，symphonia 解码，输出走 ALSA（Ubuntu 上由 PipeWire 接管）
- 递归扫描目录、搜索过滤、双击播放、上一首/下一首、进度拖拽、音量
- 三种模式：列表循环 / 单曲循环 / 关闭循环，另有随机播放
- 退出时记住窗口尺寸、音量、上次播放的曲目与进度，下次打开可以接着听

**歌词**
- 优先读同名 `.lrc` → 音频内嵌歌词 → 本地缓存 → 在线（LRCLIB）
- 正在唱的那句**始终停在歌词控件的中线**：位置由 `compute_bounds` 实测，不是按行高估算累加；
  首尾各留半个视口，第一行和最后一行也能居中
- 解析标准 LRC：多时间标签、`[offset:]`、逐字标签、原文/翻译双行合并
- 时间轴同步高亮，点击任意一行跳到对应位置
- 样式可调：字号、颜色（跟随主题或强调色）、对齐、行距、上下偏移、非当前行透明度、是否显示翻译

**标签（重点）**
- 读取：曲名、艺术家、专辑、专辑艺术家、作曲、年份、音轨/碟号、流派、内嵌封面、内嵌歌词
- **第一步就分派到标签方言**（`src/meta/`）：先判断文件用的是哪种标签
  （ID3v2.2 / 2.3 / 2.4、ID3v1、VorbisComment、MP4 ilst、APEv2、RIFF INFO），
  再由该方言给出**每个字段**的记号。不是「读年份时特判一套、读作曲时再猜一套」：

  | 字段 | v2.2 | v2.3 | v2.4 | Vorbis | MP4 | APEv2 |
  |---|---|---|---|---|---|---|
  | 曲名 | TT2 | TIT2 | TIT2 | TITLE | ©nam | Title |
  | 艺术家 | TP1 | TPE1 | TPE1 | ARTIST | ©ART | Artist |
  | 专辑 | TAL | TALB | TALB | ALBUM | ©alb | Album |
  | 作曲 | TCM | TCOM | TCOM | COMPOSER | ©wrt | Composer |
  | 年份 | TYE | TYER | TDRC | DATE / YEAR | ©day | Year / Date |
  | 音轨 | TRK | TRCK | TRCK | TRACKNUMBER | trkn | Track |
  | 歌词 | ULT | USLT | USLT | LYRICS | ©lyr | Lyrics |
  | 封面 | PIC | APIC | APIC | 图片块 | covr | Cover Art |

- 读不到才回原始字节：`TXXX:YEAR` 这类私货、v2.2 的 3 字节帧头、甚至 lofty
  整个解析失败的怪文件，都由 ID3v2 方言自己按帧扫描（支持去同步、扩展头、
  编码 0/1/2/3），主标签缺的字段再落到副标签（老 MP3 常见：v2 没年份、v1 才有）
- **写入是单一入口、统一升级**：`meta::write_meta` 是所有字段修改的唯一公共入口。
  只要改任意一个字段（曲名、年份、歌词、在线匹配整批写入……），ID3 家族都会把
  整张标签**重建为 ID3v2.4**：
  - v2.2 的三字节帧 ID 全部换成 v2.4 的四字节写法，`PIC` 转 `APIC`，
    `TORY` 转 `TDOR`，`IPLS` 转 `TIPL`；v2.4 已废弃的 `TSIZ`/`RVAD`/`EQUA` 丢弃
  - 各版本的年份写法（`TYE` / `TYER`+`TDAT`+`TIME` / `TDRC` / `TXXX:YEAR`）
    合并成一条 `TDRC`，原 `TDRC` 带月日时保留完整日期
  - 文件尾的 ID3v1 先把独有字段并入 v2.4，然后删除，只留一张标签
  - 其余不相关帧原样保留（只换帧头），音轨号 `3/10` 里的总数不会丢
- 其它容器用它自己的标准标签（FLAC/OGG → VorbisComment、M4A → ilst、APE → APEv2），
  因为这些格式存不了 ID3v2.4
- 写完统一读回校验；报告里会说明「从 v2.3 升级到 v2.4」「移除了 ID3v1」这类动作，
  存不下的字段（如 ID3v1 存不了中文）也会明确列出，而不是写一堆乱码还报成功
- 右键菜单 →「在线匹配元数据…」：iTunes Search + MusicBrainz 双源检索，先预览候选（封面、专辑、年份），选中后再决定写不写
- 右键菜单 →「编辑标签…」：手动改曲名/艺术家/专辑/作曲/流派/年份，替换或删除封面，直接粘贴 LRC 歌词；保存时只提交真正改过的字段
- **写入策略**：编辑框里「同时写入音频文件」默认关闭 —— 关闭时只改当前运行中的显示，一个字节都不碰文件；打开才写回
- 写回失败（只读文件、权限不足）只会提示，不会崩

**界面**
- 顶栏按钮：添加文件夹、重新扫描、设置；系统自带的最小化/最大化/关闭
- 设置里可以：切深色/浅色、强调色（可跟随封面主色）、歌单放左侧或右侧、歌词背景用不用封面、背景模糊强度、**背景不透明度**、无歌词时是否自动在线匹配
- 右键菜单还支持「在文件夹中显示」「从列表移除」（移除只是移出列表，文件不动）

## 编译

需要 Rust 1.75+，以及 GTK4 的开发文件：

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libasound2-dev
cargo build --release
```

产物在 `target/release/owlmusic`。

> 如果系统里只装了 GTK4 的运行时（没有 `libgtk-4-dev`），`pkg-config` 会找不到 `gdk4.pc / gsk4.pc`。
> 项目里带了一个 `.devdeps/` 兼容层，可以直接：
> ```bash
> PKG_CONFIG_PATH="$PWD/.devdeps/pc" cargo build --release
> ```
> 它只是补两份 pkg-config 描述，指向系统已有的 `libgtk-4.so`，对产物没有影响。

## 运行

```bash
./target/release/owlmusic
```

首次打开点右上角文件夹图标选一个音乐目录，会递归扫描。配置写在 `~/.config/owlmusic/config.json`，封面/歌词缓存在 `~/.cache/owlmusic/`（都遵循 `XDG_CONFIG_HOME` / `XDG_CACHE_HOME`）。

快捷键：`空格` 播放/暂停，`← →` 前后 5 秒。右键歌单行可以「在线匹配元数据 / 编辑标签 / 在文件夹中显示 / 从列表移除」。

命令行自检（不打开窗口，用来确认解码、标签、歌词、网络是否正常）：

```bash
owlmusic --selftest --dir ~/Music
```

它会打印配置路径、libasound 是否可用、扫描到多少首、前几首读到的标签，再拿第一首去 LRCLIB 和 iTunes 各试一次。

## 打包成 deb

```bash
./build-deb.sh            # 版本号默认取 Cargo.toml
sudo dpkg -i dist/owlmusic_0.1.0_amd64.deb
```

装了之后会出现在应用菜单里，`audio/*` 的 MIME 也注册了（文件管理器里右键「用 OwlMusic 打开」）。卸载：`sudo apt remove owlmusic`。

## 目录结构

```
src/
  main.rs       入口，GTK Application 与激活流程
  config.rs     配置结构与 XDG 路径
  library.rs    目录扫描、标签读取、曲库缓存
  player.rs     解码线程、播放状态机、seek/音量
  alsa.rs       dlopen libasound 输出（不需要 alsa 头文件）
  tags.rs       标签读写的对外门面（薄封装，实现都在 meta/）
  meta/         标签方言层：一次分派，所有字段按对应格式的记号读写
    mod.rs      TagDialect trait、Field/TrackMeta/MetaPatch/WriteReport、
                按 TagType 分派、跨标签兜底与写后校验
    id3v2.rs    2.2/2.3/2.4 全字段读写 + 原始帧扫描 + 统一重建为 v2.4 的写入器
    id3v1.rs    末尾 128 字节的定长字段（中文会被明确跳过）
    vorbis.rs   TITLE/ARTIST/ALBUM/COMPOSER/DATE/... 
    mp4.rs      ©nam/©ART/©alb/©wrt/©day/trkn/...
    ape.rs      Title/Artist/Composer/Year/...
    generic.rs  RIFF INFO / AIFF 兜底
  online.rs     iTunes / MusicBrainz / LRCLIB 检索与缓存
  lrc.rs        LRC 解析、序列化、同步定位
  ui/
    mod.rs      主窗口、歌单、歌词、播放条、右键菜单
    dialogs.rs  设置、歌词样式、在线匹配、手动编辑标签
    art.rs      封面圆角、模糊背景、主色提取（纯 CPU）
    theme.rs    CSS 变量表：浅/深色 + 强调色
resources/      .desktop / 图标 / AppStream 元数据
build-deb.sh    dpkg-deb 打包脚本
```

## 一些实现上的取舍

- **不用 ffmpeg / gstreamer**：symphonia 纯 Rust 解码，`cargo build` 之后只依赖系统里本来就有的 `libgtk-4` 和 `libasound`。
- **ALSA 用 dlopen 调**：`libasound.so.2` 在 Ubuntu 上必然存在，但开发头文件不一定，直接 `libloading` 取符号可以省掉 `libasound2-dev`。
- **模糊背景在 CPU 上做**：封面先降到 128×128 再做两轮 box blur，比每帧跑一次实时模糊省太多。
  这块模糊图只铺在**歌词区**（`gtk::Overlay` 的底层，圆角裁剪），不透明度独立可调。
- **写标签默认关闭**：误改用户音乐文件是不可接受的副作用，所以默认只在内存里生效，必须显式打开开关。

## 已知限制

- 没有 MPRIS，所以 GNOME 顶栏和耳机按键还控制不到它（下一步最值得加的）。
- 没有均衡器、没有 m3u 导入导出。
- 在线检索依赖 LRCLIB / iTunes / MusicBrainz 的可用性，国内访问可能不稳；失败时会退回本地标签，不会卡住界面。

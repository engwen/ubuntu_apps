/// 日志捕获模块：捕获应用日志 + RSSI 历史供诊断查看
use log::{Level, LevelFilter, Log, Metadata, Record, SetLoggerError};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::Mutex;

const MAX_LOG_ENTRIES: usize = 500;
const MAX_RSSI_POINTS: usize = 120;

static LOG_BUFFER: Mutex<Option<VecDeque<String>>> = Mutex::new(None);
static RSSI_HISTORY: Mutex<VecDeque<i32>> = Mutex::new(VecDeque::new());

pub fn init() -> Result<(), SetLoggerError> {
    *LOG_BUFFER.lock().unwrap() = Some(VecDeque::with_capacity(256));
    // 默认只记录 Info 及以上：Debug 日志会让每轮轮询都格式化字符串并写 stderr，
    // 对常驻后台程序是纯开销。需要排查问题时用 BT_LOCK_LOG=debug 打开。
    let level = match std::env::var("BT_LOCK_LOG").as_deref() {
        Ok("debug") | Ok("trace") => LevelFilter::Debug,
        _ => LevelFilter::Info,
    };
    log::set_boxed_logger(Box::new(CaptureLogger))
        .map(|()| log::set_max_level(level))
}

/// 记录一个 RSSI 数据点
pub fn push_rssi(rssi: i32) {
    let mut buf = RSSI_HISTORY.lock().unwrap();
    if buf.len() >= MAX_RSSI_POINTS {
        buf.pop_front();
    }
    buf.push_back(rssi);
}

/// 获取 RSSI 历史数据的克隆
pub fn get_rssi_history() -> Vec<i32> {
    RSSI_HISTORY.lock().unwrap().iter().cloned().collect()
}

pub fn get_logs() -> String {
    let guard = LOG_BUFFER.lock().unwrap();
    match guard.as_ref() {
        Some(buf) => buf.iter().cloned().collect::<Vec<_>>().join("\n"),
        None => String::new(),
    }
}

/// 生成 RSSI 信号曲线的文本可视化（带移动平均平滑）
pub fn render_rssi_curve(history: &[i32], width: usize, height: usize) -> String {
    if history.is_empty() {
        return "暂无 RSSI 数据，启动监控后自动采集".to_string();
    }

    // 取最后 width 个点
    let raw: Vec<i32> = if history.len() > width {
        history[history.len() - width..].to_vec()
    } else {
        history.to_vec()
    };

    // 移动平均平滑（窗口=3）
    let data: Vec<f64> = raw
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let start = if i >= 2 { i - 2 } else { 0 };
            let slice = &raw[start..=i];
            slice.iter().sum::<i32>() as f64 / slice.len() as f64
        })
        .collect();

    let min_val = data.iter().cloned().fold(f64::INFINITY, f64::min).min(-100.0);
    let max_val = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max).max(-30.0);
    let range = (max_val - min_val).max(1.0);

    // 构建画布
    let mut canvas = vec![vec![' '; width]; height];

    for (x, &val) in data.iter().enumerate() {
        let normalized = (val - min_val) / range;
        let y = ((1.0 - normalized) * (height - 1) as f64).round() as usize;
        let y = y.min(height - 1);
        canvas[y][x] = '●';
        // 连线上下的点
        for row in 0..height {
            if canvas[row][x] == ' ' {
                if row > 0 && canvas[row - 1][x] == '●' {
                    canvas[row][x] = '│';
                } else if row + 1 < height && canvas[row + 1][x] == '●' {
                    canvas[row][x] = '│';
                }
            }
        }
    }

    let mut lines = Vec::new();
    for (row_idx, row) in canvas.iter().enumerate() {
        let label = match row_idx {
            0 => format!("{:>4} |", max_val as i32),
            r if r == height / 2 => format!("{:>4} |", ((max_val + min_val) / 2.0) as i32),
            r if r == height - 1 => format!("{:>4} |", min_val as i32),
            _ => "     |".to_string(),
        };
        let line: String = row.iter().collect();
        lines.push(format!("{label}{line}"));
    }
    lines.push(format!("     +{}", "-".repeat(width)));
    lines.push(format!("      {}个数据点，最新: {:.0} dBm (平滑)", data.len(), data.last().unwrap_or(&0.0)));
    lines.join("\n")
}

struct CaptureLogger;

impl Log for CaptureLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let ts = chrono_like_timestamp();
        let level = match record.level() {
            Level::Error => "ERROR",
            Level::Warn => " WARN",
            Level::Info => " INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };
        let msg = format!("{ts} {level}  {}", record.args());

        // 输出到 stderr（使用 write_all 避免递归触发 log 宏）
        let _ = std::io::stderr().write_all(msg.as_bytes());
        let _ = std::io::stderr().write_all(b"\n");

        // 写入有界缓冲区（msg 直接移动，省掉一次字符串克隆）
        if let Some(buf) = LOG_BUFFER.lock().unwrap().as_mut() {
            if buf.len() >= MAX_LOG_ENTRIES {
                let drain = buf.len() - MAX_LOG_ENTRIES + 64;
                buf.drain(..drain);
            }
            buf.push_back(msg);
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// 简单的时间戳生成（UTC+8 东八区）
fn chrono_like_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // 转换为东八区（+8 小时）
    let secs_cst = secs + 8 * 3600;
    let days = secs_cst / 86400;
    let time_of_day = secs_cst % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    let (y, mo, d) = epoch_days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}+08")
}

fn epoch_days_to_ymd(days: u64) -> (u64, u64, u64) {
    // 简化的日期计算
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// 智能自适应蓝牙轮询器
/// 状态变化时加速轮询（0.5s），状态稳定后恢复配置值

/// 加速轮询间隔（毫秒）
const FAST_POLL_MS: u32 = 500;
/// 连续低于阈值的次数阈值，防止信号毛刺误判
const LOW_RSSI_CONFIRM_COUNT: i32 = 3;

pub struct SmartPoller {
    threshold: i32,
    scan_interval: i32,
    active: bool,
    low_count: i32,
    /// 状态变化时为true，轮询加速到0.5s
    fast_poll: bool,
    /// 加速轮询持续的 tick 数
    fast_poll_ticks: i32,
    /// 上一次的连接状态，用于检测变化
    last_connected: bool,
}

impl SmartPoller {
    pub fn new(threshold: i32, scan_interval: i32) -> Self {
        Self {
            threshold,
            scan_interval,
            active: false,
            low_count: 0,
            fast_poll: false,
            fast_poll_ticks: 0,
            last_connected: true,
        }
    }

    pub fn set_threshold(&mut self, threshold: i32) {
        self.threshold = threshold;
    }

    pub fn set_scan_interval(&mut self, interval: i32) {
        self.scan_interval = interval;
    }

    pub fn start(&mut self) {
        self.active = true;
        self.low_count = 0;
        self.fast_poll = false;
        self.fast_poll_ticks = 0;
        self.last_connected = true;
    }

    pub fn stop(&mut self) {
        self.active = false;
        self.low_count = 0;
        self.fast_poll = false;
        self.fast_poll_ticks = 0;
    }

    /// 手动触发加速轮询（状态变化时调用）
    pub fn trigger_fast_poll(&mut self) {
        self.fast_poll = true;
    }

    /// 是否处于加速轮询状态
    pub fn is_fast_polling(&self) -> bool {
        self.fast_poll
    }

    /// 根据当前连接状态和 RSSI 返回下次轮询间隔（毫秒）
    /// 返回 (poll_ms, should_trigger_disconnect)
    pub fn tick(&mut self, connected: bool, rssi: i32, rssi_supported: bool) -> (u32, bool) {
        if !self.active {
            return (self.scan_interval as u32 * 1000, false);
        }

        // 检测连接状态变化，触发加速轮询
        if connected != self.last_connected {
            self.fast_poll = true;
            self.fast_poll_ticks = 0;
            self.last_connected = connected;
            log::debug!(
                "连接状态变化: {} -> {}，加速轮询",
                !connected, connected
            );
        }

        // 加速轮询 tick 计数，稳定后恢复
        if self.fast_poll {
            self.fast_poll_ticks += 1;
            if self.fast_poll_ticks >= 6 {
                self.fast_poll = false;
                self.fast_poll_ticks = 0;
                log::debug!("加速轮询结束（6 tick），恢复常规轮询");
            }
        }

        if !connected {
            self.low_count = 0;
            let ms = if self.fast_poll {
                FAST_POLL_MS
            } else {
                self.scan_interval as u32 * 1000
            };
            return (ms, true);
        }

        // RSSI 不支持时跳过阈值判断
        if rssi == 0 || !rssi_supported {
            let ms = if self.fast_poll {
                FAST_POLL_MS
            } else {
                self.scan_interval as u32 * 1000
            };
            return (ms, false);
        }

        // 低信号检测
        if rssi < self.threshold {
            self.low_count += 1;
            self.fast_poll = true; // 低于阈值时加速
            self.fast_poll_ticks = 0;
            log::debug!(
                "低信号计数: {}/{}，加速轮询",
                self.low_count, LOW_RSSI_CONFIRM_COUNT
            );
            if self.low_count >= LOW_RSSI_CONFIRM_COUNT {
                self.low_count = 0;
                return (FAST_POLL_MS, true);
            }
        } else {
            // 信号恢复
            self.low_count = 0;
        }

        // 正常监控
        let ms = if self.fast_poll {
            FAST_POLL_MS
        } else {
            self.scan_interval as u32 * 1000
        };
        (ms, false)
    }
}

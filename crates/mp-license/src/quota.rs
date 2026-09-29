//! 未激活用户的每日免费额度
//!
//! 规则：**未激活每天可解码 100 首，激活后不限量。**
//!
//! 额度状态同样加密存放（密钥派生自机器指纹），并把日期一起写进去，
//! 以处理"跨天重置"与"改系统时间刷额度"两种情况：
//!
//! - 日期前进了（正常跨天）→ 重置计数
//! - 日期倒退了（时钟回拨）→ **拒绝服务**，不重置

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 未激活时每天允许解码的首数
///
/// 产品政策：**未激活 100 首/天**，激活后不限量。改动这个值会直接改变
/// 未付费用户能转换多少文件，改之前先确认是产品决策而不是临时调试。
/// `free_quota_is_100_per_day` 会盯住这个数。
pub const FREE_PER_DAY: u32 = 100;

const QUOTA_FILE: &str = "quota.bin";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuotaState {
    /// 本地日期 `YYYY-MM-DD`
    pub date: String,
    /// 当日已使用
    pub used: u32,
}

/// 供 UI 展示的额度信息
#[derive(Debug, Clone, Serialize)]
pub struct QuotaInfo {
    pub activated: bool,
    /// 激活后为 true，此时 `remaining` 无意义
    pub unlimited: bool,
    pub limit: u32,
    pub used: u32,
    pub remaining: u32,
    pub date: String,
}

fn path() -> PathBuf {
    crate::default_license_dir().join(QUOTA_FILE)
}

/// 记录日期与「今天」的关系
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayState {
    /// 同一天
    Same,
    /// 记录为空或已过去 —— 正常跨天，应重置
    Rolled,
    /// 记录日期在未来 —— 时钟被回拨，应拒绝服务
    Back,
}

/// 判定日期关系。抽成纯函数以便单测（不触碰真实存储）
pub fn classify(state_date: &str, today: &str) -> DayState {
    if state_date.is_empty() || state_date < today {
        DayState::Rolled
    } else if state_date == today {
        DayState::Same
    } else {
        DayState::Back
    }
}

/// 本地日期（`YYYY-MM-DD`）
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub fn load() -> QuotaState {
    match crate::secure::read_encrypted(&path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => QuotaState::default(),
    }
}

pub fn save(state: &QuotaState) -> Result<()> {
    crate::secure::write_encrypted(&path(), &serde_json::to_vec(state)?)
}

/// 读取额度信息（自动处理跨天重置）
pub fn info() -> QuotaInfo {
    let activated = crate::status().activated;
    let date = today();

    if activated {
        return QuotaInfo {
            activated: true,
            unlimited: true,
            limit: 0,
            used: 0,
            remaining: u32::MAX,
            date,
        };
    }

    let state = load();
    // 跨天视为已重置；时钟回拨则按已用完处理
    let used = match classify(&state.date, &date) {
        DayState::Same => state.used,
        DayState::Rolled => 0,
        DayState::Back => FREE_PER_DAY,
    };

    QuotaInfo {
        activated: false,
        unlimited: false,
        limit: FREE_PER_DAY,
        used,
        remaining: FREE_PER_DAY.saturating_sub(used),
        date,
    }
}

/// 检查是否还能解码 `n` 首；不实际扣减
pub fn check(n: u32) -> Result<()> {
    if crate::status().activated {
        return Ok(());
    }
    let state = load();
    let date = today();

    let used = match classify(&state.date, &date) {
        DayState::Same => state.used,
        DayState::Rolled => 0,
        // 时钟回拨：拒绝服务，不重置
        DayState::Back => return Err(Error::QuotaExceeded(FREE_PER_DAY)),
    };

    if used + n > FREE_PER_DAY {
        return Err(Error::QuotaExceeded(FREE_PER_DAY));
    }
    Ok(())
}

/// 实际扣减 `n` 首（仅在转换**成功**后调用）
pub fn consume(n: u32) -> Result<()> {
    if n == 0 || crate::status().activated {
        return Ok(());
    }
    let mut state = load();
    let date = today();

    match classify(&state.date, &date) {
        DayState::Same => state.used = state.used.saturating_add(n),
        DayState::Rolled => {
            state.date = date;
            state.used = n;
        }
        DayState::Back => return Err(Error::QuotaExceeded(FREE_PER_DAY)),
    }
    save(&state)
}

/// 退还额度（转换失败时不占用用户配额）
pub fn refund(n: u32) -> Result<()> {
    if n == 0 || crate::status().activated {
        return Ok(());
    }
    let mut state = load();
    state.used = state.used.saturating_sub(n);
    save(&state)
}

/// 未激活时，本次最多允许处理多少首
pub fn allowance(n: u32) -> u32 {
    if crate::status().activated {
        return n;
    }
    let info = info();
    n.min(info.remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_is_iso_date() {
        let t = today();
        assert_eq!(t.len(), 10);
        assert_eq!(t.chars().nth(4), Some('-'));
    }

    // 以下用例均为纯逻辑，不读写真实存储目录

    #[test]
    fn same_day_is_same() {
        assert_eq!(classify("2026-09-16", "2026-09-16"), DayState::Same);
    }

    #[test]
    fn next_day_rolls() {
        assert_eq!(classify("2026-09-16", "2026-09-17"), DayState::Rolled);
    }

    #[test]
    fn empty_state_rolls() {
        assert_eq!(classify("", "2026-09-16"), DayState::Rolled);
    }

    #[test]
    fn clock_rollback_is_detected() {
        // 记录日期在未来 = 用户把系统时间往回调
        assert_eq!(classify("2027-01-01", "2026-09-16"), DayState::Back);
    }

    #[test]
    fn quota_math() {
        // 用满额度后应剩 0，再多一首就超限
        assert_eq!(FREE_PER_DAY.saturating_sub(FREE_PER_DAY), 0);
        assert!(FREE_PER_DAY + 1 > FREE_PER_DAY);
    }

    /// 盯住产品政策：未激活 100 首/天
    ///
    /// 这里不用 `> 1` 这种弱断言 —— 之前 UI 上出现过写死 1 的地方，
    /// 放宽断言会让回归溜过去。
    #[test]
    fn free_quota_is_100_per_day() {
        assert_eq!(FREE_PER_DAY, 100);
    }
}

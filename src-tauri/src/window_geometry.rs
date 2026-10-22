//! 主窗口的默认几何计算（UI-1 第 1 节）：都不碰系统调用，纯函数方便单测。
//!
//! 规则（任务卡给的）：
//! - 默认 1610×946；屏幕放不下时取"1610×946"和**工作区**（去掉任务栏）90% 中
//!   较小的那个，居中显示；
//! - 最小尺寸 900×600；工作区比这还小时，最小尺寸也按工作区的 90% 放宽
//!   （不能出现窗口比屏幕还大、拖不动的情况）；
//! - 保存的位置不在任何屏幕上时回主屏幕居中。
//!
//! 全部用**逻辑像素**（调用方把物理工作区按缩放折算好再进来）。

/// 默认窗口大小（用户 2026-10-21 定的：2560×1440 屏上用着顺手的大小）。这是内容区尺寸；
/// Windows 量窗口外框（含看不见的缩放边）会多出 16×9，即 1626×955。
pub const DEFAULT_WIDTH: u32 = 1610;
pub const DEFAULT_HEIGHT: u32 = 946;

/// 参考 Netcatty `mainWindow.cjs` 的 `MIN_SIZE_FIT_FACTOR`。
pub const MIN_SIZE_FIT_FACTOR: f64 = 0.9;

/// 最小尺寸（正常屏幕上）。
pub const MIN_WIDTH: u32 = 900;
pub const MIN_HEIGHT: u32 = 600;

/// 一个矩形（位置 + 尺寸，逻辑像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 窗口要用的几何：尺寸 + 位置 + 最小尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub min_width: u32,
    pub min_height: u32,
}

/// 取工作区 90%（至少 1 像素，防止极端小屏算出 0）。
fn ninety_percent(value: u32) -> u32 {
    ((value as f64) * MIN_SIZE_FIT_FACTOR).round().max(1.0) as u32
}

/// 默认窗口大小：1610×946 和工作区 90% 取小的那个。
pub fn fit_default_size(work_width: u32, work_height: u32) -> (u32, u32) {
    (
        DEFAULT_WIDTH.min(ninety_percent(work_width)).max(1),
        DEFAULT_HEIGHT.min(ninety_percent(work_height)).max(1),
    )
}

/// 最小尺寸：900×600；工作区更小时按工作区 90% 放宽（也保证不比默认尺寸大）。
pub fn fit_min_size(work_width: u32, work_height: u32) -> (u32, u32) {
    let (default_width, default_height) = fit_default_size(work_width, work_height);
    (
        MIN_WIDTH
            .min(ninety_percent(work_width))
            .min(default_width)
            .max(1),
        MIN_HEIGHT
            .min(ninety_percent(work_height))
            .min(default_height)
            .max(1),
    )
}

/// 在工作区里居中。
pub fn center_in(work: Rect, width: u32, height: u32) -> (i32, i32) {
    let x = work.x + ((work.width as i64 - width as i64) / 2) as i32;
    let y = work.y + ((work.height as i64 - height as i64) / 2) as i32;
    (x, y)
}

/// 没有保存过状态时的几何：适配工作区 + 居中。
pub fn default_geometry(work: Rect) -> WindowGeometry {
    let (width, height) = fit_default_size(work.width, work.height);
    let (min_width, min_height) = fit_min_size(work.width, work.height);
    let (x, y) = center_in(work, width, height);
    WindowGeometry {
        x,
        y,
        width,
        height,
        min_width,
        min_height,
    }
}

/// 两个矩形有没有重叠（严格重叠，贴边不算）。
pub fn overlaps(a: Rect, b: Rect) -> bool {
    let a_right = a.x as i64 + a.width as i64;
    let a_bottom = a.y as i64 + a.height as i64;
    let b_right = b.x as i64 + b.width as i64;
    let b_bottom = b.y as i64 + b.height as i64;
    (a.x as i64) < b_right
        && (b.x as i64) < a_right
        && (a.y as i64) < b_bottom
        && (b.y as i64) < a_bottom
}

/// 窗口跟任何一个屏幕有没有重叠（没有就说明保存的位置跑到屏幕外了）。
pub fn intersects_any(window: Rect, monitors: &[Rect]) -> bool {
    monitors.iter().any(|monitor| overlaps(window, *monitor))
}

/// 保存的尺寸比当前工作区还大（上次在大屏、这次换了小屏）时，那一边缩到工作区的 90%；
/// 放得下就原样保留——**不能**按默认尺寸封顶，否则用户调大的窗口每次启动都被缩回默认大小。
/// 位置不动——位置由 [`intersects_any`] 单独判。
pub fn clamp_size_to_work_area(width: u32, height: u32, work: Rect) -> (u32, u32) {
    let fit = |value: u32, limit: u32| {
        if value > limit {
            ninety_percent(limit)
        } else {
            value.max(1)
        }
    };
    (fit(width, work.width), fit(height, work.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(width: u32, height: u32) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn default_size_fits_common_work_areas() {
        // 大屏：用默认 1610×946。
        assert_eq!(fit_default_size(2560, 1440), (1610, 946));
        assert_eq!(fit_default_size(2560, 1392), (1610, 946));
        // 1366×768 笔记本：宽 1610 放不下 → 1366*0.9=1229；高 946 放不下 → 768*0.9=691。
        assert_eq!(fit_default_size(1366, 768), (1229, 691));
        // 1080p@150%（逻辑 1280×720）：两边都缩。
        assert_eq!(fit_default_size(1280, 720), (1152, 648));
        // 极端小屏不会算出 0。
        assert_eq!(fit_default_size(200, 150), (180, 135));
    }

    #[test]
    fn min_size_relaxes_on_small_work_areas() {
        assert_eq!(fit_min_size(2560, 1440), (900, 600));
        assert_eq!(fit_min_size(1366, 768), (900, 600));
        // 1280×720：900 还能放下，600 也能。
        assert_eq!(fit_min_size(1280, 720), (900, 600));
        // 960×540（很小的逻辑屏）：min 按 90% 放宽 → 864×486。
        assert_eq!(fit_min_size(960, 540), (864, 486));
        // 更极端：min 不能超过默认尺寸。
        let (min_w, min_h) = fit_min_size(800, 600);
        let (w, h) = fit_default_size(800, 600);
        assert!(min_w <= w && min_h <= h);
    }

    #[test]
    fn default_geometry_is_centered() {
        // 2560×1392 工作区（1440 减任务栏）：1610×946 居中。
        let geometry = default_geometry(work(2560, 1392));
        assert_eq!((geometry.width, geometry.height), (1610, 946));
        assert_eq!(
            (geometry.x, geometry.y),
            ((2560 - 1610) / 2, (1392 - 946) / 2)
        );

        // 1920×1040 工作区（1080 减任务栏）：宽 1610 放得下；高 946 超过 90%（936）→ 1610×936，居中。
        let geometry = default_geometry(work(1920, 1040));
        assert_eq!((geometry.width, geometry.height), (1610, 936));
        assert_eq!((geometry.x, geometry.y), (155, 52));
        assert_eq!((geometry.min_width, geometry.min_height), (900, 600));

        // 1366×768：1229×691 居中。
        let geometry = default_geometry(work(1366, 768));
        assert_eq!((geometry.width, geometry.height), (1229, 691));
        assert_eq!(geometry.x, (1366 - 1229) / 2);
        assert_eq!(geometry.y, (768 - 691) / 2);

        // 副屏工作区（左上角不在 0,0）也要按它居中。
        let second = Rect {
            x: 1920,
            y: -200,
            width: 1920,
            height: 1040,
        };
        let geometry = default_geometry(second);
        assert_eq!(geometry.x, 1920 + 155);
        assert_eq!(geometry.y, -200 + 52);
    }

    #[test]
    fn off_screen_positions_are_detected() {
        let monitors = [
            work(1920, 1080),
            Rect {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
        ];
        // 在主屏里。
        assert!(intersects_any(
            Rect {
                x: 100,
                y: 100,
                width: 1200,
                height: 800
            },
            &monitors
        ));
        // 在副屏（右半部分）。
        assert!(intersects_any(
            Rect {
                x: 2000,
                y: 50,
                width: 1200,
                height: 800
            },
            &monitors
        ));
        // 副屏拔了：现在只剩主屏，位置在 x=2400 → 没有任何屏幕。
        assert!(!intersects_any(
            Rect {
                x: 2400,
                y: 50,
                width: 1200,
                height: 800
            },
            &[work(1920, 1080)]
        ));
        // 负方向跑出去。
        assert!(!intersects_any(
            Rect {
                x: -3000,
                y: 0,
                width: 1200,
                height: 800
            },
            &monitors
        ));
        // 贴边（完全在屏幕右侧外面）不算重叠。
        assert!(!intersects_any(
            Rect {
                x: 1920,
                y: 0,
                width: 1200,
                height: 800
            },
            &[work(1920, 1080)]
        ));
    }

    #[test]
    fn saved_size_is_clamped_when_the_screen_shrank() {
        // 上次在 2560×1440 用了 2000×1200，这次只有 1366×768 → 缩到 90% 内。
        let (width, height) = clamp_size_to_work_area(2000, 1200, work(1366, 768));
        assert_eq!((width, height), (1229, 691));
        // 正常放得下就不动。
        let (width, height) = clamp_size_to_work_area(1200, 800, work(1920, 1040));
        assert_eq!((width, height), (1200, 800));
        // 比默认 1610×946 大、但工作区放得下：原样保留（以前被缩回默认大小）。
        let (width, height) = clamp_size_to_work_area(1857, 1086, work(2560, 1392));
        assert_eq!((width, height), (1857, 1086));
        // 只有一边超出：只缩那一边。
        let (width, height) = clamp_size_to_work_area(1500, 1300, work(1920, 1040));
        assert_eq!((width, height), (1500, 936));
    }
}

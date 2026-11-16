//! 文件作用：长截图流程的帧匹配子模块——估计相邻两帧之间的滚动位移，
//! 自动识别顶部/底部固定栏，并探测画面是否发生运动。
//! 所属业务模块：长截图工具（scrollshot）的匹配层。
//! 核心结构体/函数清单：Features（一帧的匹配特征）、Motion（一次匹配结果）、
//! detect_fixed_bands（固定栏识别）、probe_has_motion（运动探测）。
//! 依赖能力：仅依赖 frame 模块的 RgbFrame 亮度接口，纯内存计算。
//! 业务目标：帧匹配=找出两帧画面间的对应关系。这里把“对应关系”具体化为
//! “下一帧相对当前帧向上滚动了多少行”，这是拼接模块追加新条带长度的依据；
//! 同时利用“滚动时固定栏像素不变、正文像素变化”的差异统计固定栏边界。
use crate::{Crop, RgbFrame};

/// 每帧亮度采样列数：把正文区域横向压缩成 96 个采样列，既保留纵向纹理
/// 又大幅降低比较成本。
const COLUMNS: usize = 96;
/// 粗匹配的纵向降采样倍率：每 4 行亮度合并成 1 行，用于快速粗定位滚动位移。
const SCALE: usize = 4;

/// 一帧的匹配特征（帧匹配用到的压缩表示）。
/// 业务定位：匹配算法不直接比较原始帧，而是比较两帧的 Features，
/// 把“找滚动位移”转化为“在特征网格上找最小差异的纵向偏移”。
pub(crate) struct Features {
    /// 全分辨率亮度采样网格：共 height 行 × 96 列，每字节一个 0~255 亮度值。
    /// 每列取自帧内一条竖直采样线，覆盖正文宽度的约 8%~77%（避开两侧边缘）。
    samples: Vec<u8>,
    /// 粗网格：把 samples 每 4 行取平均，供低成本的粗匹配阶段使用。
    coarse: Vec<u8>,
    /// 正文区域高度（行数，已扣除上下固定栏）。
    height: usize,
    /// 背景亮度：采样值的中位数，代表画面底色，用于衡量纹理强度。
    background: u8,
}

/// 一次帧匹配的结果。
/// 业务定位：shift 为“下一帧相对当前帧向上滚动的行数”，是拼接长度的直接依据；
/// error 与 texture 用于判断这次匹配是否可靠。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Motion {
    /// 滚动位移：正值表示内容上移（读到了更靠下的内容）。
    pub shift: i32,
    /// 归一化匹配误差：重叠区域差异能量与纹理信号能量的比值，越接近 0 越吻合。
    pub error: f64,
    /// 重叠区域平均纹理强度：内容越“有细节”越大；纯色区域会很小。
    pub texture: f64,
}

impl Features {
    /// 将裁剪后的正文转换为稀疏亮度特征，降低相邻帧滚动匹配成本。
    /// 入参：frame:&RgbFrame 原始帧；crop:Crop，必须先校验合法且正文非空。
    /// 返回值：Features，samples 为正文高×96 列，coarse 为每四行平均的网格，background 为采样亮度累计中点。
    /// 异常：不返回 Result，非法裁剪或像素访问可 panic，分配失败不可保证恢复。
    /// 注意事项：横向只取约 8% 至 77% 区域；coarse 丢弃不足四行的尾部。计算量主要随正文高度×固定采样列数增长，不是逐原始像素处理。
    pub fn new(frame: &RgbFrame, crop: Crop) -> Self {
        let width = (frame.width() - crop.left - crop.right) as usize;
        let height = (frame.height() - crop.top - crop.bottom) as usize;
        // 一维行优先亮度数组，每个元素对应当前正文行上的一个采样列，不是完整 RGB 像素。
        let mut samples = Vec::with_capacity(height * COLUMNS);
        for y in 0..height {
            for column in 0..COLUMNS {
                // 排除左右边缘的滚动条和固定操作按钮，保留正文和列表序号。
                // 采样列 x 在正文宽度的 8%~77% 之间均匀分布：
                // column=0 → 8%，column=95 → (768+6650)/9600 ≈ 77.3%。
                let x = crop.left as usize + width * (8 * COLUMNS + column * 70) / (100 * COLUMNS);
                samples.push(frame.luma(x, y + crop.top as usize));
            }
        }
        // 统计亮度直方图，随后求中位数作为背景亮度。
        let mut histogram = [0_usize; 256];
        for &value in &samples {
            histogram[value as usize] += 1;
        }
        let mut count = 0;
        // 从低到高累计像素数，累计过半时对应的亮度即中位数（背景色）。
        let background = histogram
            .iter()
            .position(|n| {
                count += n;
                count >= samples.len() / 2
            })
            .unwrap_or(0) as u8;
        // 粗网格：纵向每 SCALE 行取平均，行数约为原高度的 1/4。
        let mut coarse = Vec::with_capacity(height / SCALE * COLUMNS);
        for rows in samples.chunks_exact(COLUMNS * SCALE) {
            for x in 0..COLUMNS {
                let total: u32 = (0..SCALE).map(|y| rows[y * COLUMNS + x] as u32).sum();
                coarse.push((total / SCALE as u32) as u8);
            }
        }
        Self {
            samples,
            coarse,
            height,
            background,
        }
    }

    /// 通过零位移检测及粗细两级搜索估计相邻帧滚动距离。
    /// 入参：&self 当前帧特征；next:&Self 下一帧特征，要求相同正文尺寸及一致裁剪策略。
    /// 返回值：Motion，shift 为原图行数，正表示内容上移；error/texture 为可靠性指标。没有更好候选时返回零位移分数，不保证 error 恰好为 1。
    /// 异常：无 Result，特征尺寸不匹配可导致索引 panic，分配失败不能保证恢复。
    /// 注意事项：零位移误差低于 0.015 提前接受；粗搜索范围约正文高 80%，最多六个相隔超过两粗行的峰再细化。不是全分辨率穷举或一定正确的位移。
    pub fn motion_to(&self, next: &Self) -> Motion {
        // 先试位移 0（画面静止）：误差足够小就直接认定静止，省去全局搜索。
        let zero = self.score(next, 0, false, 2, 2);
        if zero.error < 0.015 {
            return zero;
        }
        let coarse_height = self.height / SCALE;
        // 滚动位移上限：正文高度的 80%，覆盖绝大多数连续帧的重叠范围。
        let max_shift = (coarse_height * 4 / 5) as i32;
        let mut candidates = Vec::with_capacity((max_shift * 2 + 1) as usize);
        // 粗匹配阶段：在粗网格上对每个可能位移打分，纹理太弱的候选直接淘汰。
        for shift in -max_shift..=max_shift {
            let score = self.score(next, shift, true, 3, 3);
            if score.texture >= 1.0 {
                candidates.push(score);
            }
        }
        candidates.sort_unstable_by(|a, b| a.error.total_cmp(&b.error));
        // 分别细化不同的候选峰，避免列表行距产生的周期性假匹配。
        // 候选按误差升序排列；逐峰检查，只保留与已选峰相距超过 2 行的新峰，
        // 最多保留 6 个，防止在同一峰附近浪费细化次数。
        let mut peaks: Vec<i32> = Vec::new();
        for candidate in candidates {
            if peaks.iter().all(|&p| (p - candidate.shift).abs() > 2) {
                peaks.push(candidate.shift);
                if peaks.len() == 6 {
                    break;
                }
            }
        }
        let mut best = zero;
        for peak in peaks {
            // 把粗网格位移还原到原分辨率，并在 ±SCALE 邻域内逐个精细打分。
            let center = peak * SCALE as i32;
            for shift in center - SCALE as i32..=center + SCALE as i32 {
                if shift.unsigned_abs() as usize > self.height * 4 / 5 {
                    continue;
                }
                let score = self.score(next, shift, false, 2, 1);
                if score.texture >= 1.0 && score.error < best.error {
                    best = score;
                }
            }
        }
        best
    }

    /// 评估给定假想位移下的重叠画面是否一致以及纹理是否足够。
    /// 入参：&self/next:&Self 同尺寸特征；shift:i32 该网格的行位移；coarse:bool 选择四行平均或原行网格；row_step/column_step:usize 为正抽样步长。
    /// 返回值：Motion，保留传入 shift；重叠少于八行时 error=1、texture=0，其余以差异/背景偏离幅度计算。
    /// 异常：零步长会使 step_by panic，错误尺寸或非法位移可造成索引/算术错误；不返回 Result。
    /// 注意事项：粗模式 shift 是粗网格单位，调用方细化时再换算；归一化比值不是概率或固定 [0,1] 置信度，不能替代上层阈值判断。
    fn score(
        &self,
        next: &Self,
        shift: i32,
        coarse: bool,
        row_step: usize,
        column_step: usize,
    ) -> Motion {
        let (a, b, height) = if coarse {
            (&self.coarse, &next.coarse, self.height / SCALE)
        } else {
            (&self.samples, &next.samples, self.height)
        };
        // 重叠行数：两帧各去掉被滚动移出画面的部分后剩下的共同区域。
        let overlap = height.saturating_sub(shift.unsigned_abs() as usize);
        if overlap < 8 {
            // 重叠太少，匹配必然不可靠，直接返回最差分数。
            return Motion {
                shift,
                error: 1.0,
                texture: 0.0,
            };
        }
        // 当前帧中重叠区的起始行：shift>0（内容上移）时从第 shift 行开始，
        // shift<0 时从第 0 行开始；下一帧则恰好相反。
        let a_start = shift.max(0) as usize;
        let b_start = (-shift).max(0) as usize;
        // 不使用重叠区域的最上端和最下端，避开收起中的标题和边缘渐变。
        let margin = overlap / 8;
        let mut difference = 0_u64;
        let mut signal = 0_u64;
        let mut count = 0_u64;
        for y in (margin..overlap - margin).step_by(row_step) {
            for x in (0..COLUMNS).step_by(column_step) {
                let av = a[(a_start + y) * COLUMNS + x];
                let bv = b[(b_start + y) * COLUMNS + x];
                // 差异能量：两帧亮度差的绝对值之和。
                difference += av.abs_diff(bv) as u64;
                // 信号能量：两帧各自偏离背景的幅度之和，衡量该区域纹理多强。
                signal += av.abs_diff(self.background) as u64 + bv.abs_diff(next.background) as u64;
                count += 1;
            }
        }
        Motion {
            shift,
            error: difference as f64 / signal.max(1) as f64,
            texture: signal as f64 / count.max(1) as f64,
        }
    }
}

/// 从多个画面差异曲线推测不随正文滚动的顶部和底部区域。
/// 入参：frames:&[RgbFrame] 探测帧集合，通常由有运动的帧对拼接而成；遍历 windows(2) 包含相邻配对之间的边界组合。
/// 返回值：(f64,f64)，顶部/底部相对首帧高度比例；空集合、有效差异曲线少于两条或正文段过短时 (0,0)，建议不裁剪。
/// 异常：不返回 Result；不合理尺寸或像素数据可能使索引 panic，内存不足不可保证恢复。
/// 注意事项：尺寸不符的相邻窗口跳过；取逐行差异中位数和四行均值阈值，再找允许小间隔的最长活跃段。顶部封顶 45%、底部 35%，是启发式估计，不保证真正识别 UI 边界。
pub(crate) fn detect_fixed_bands(frames: &[RgbFrame]) -> (f64, f64) {
    let Some(first) = frames.first() else {
        return (0.0, 0.0);
    };
    let height = first.height() as usize;
    let width = first.width() as usize;
    // 每对相邻帧产出一份“逐行平均亮度差”曲线，存在差异明显的帧对才参与统计。
    // 每项为一对有效相邻帧的逐行差异数组；每个字段位置 y 对应源帧第 y 行。
    let mut differences = Vec::new();
    for pair in frames.windows(2) {
        if pair
            .iter()
            .any(|frame| frame.width() != first.width() || frame.height() != first.height())
        {
            continue;
        }
        // 对每一行，在 48 个采样列上求两帧亮度差的平均值，得到该行“变化程度”。
        let rows: Vec<f64> = (0..height)
            .map(|y| {
                (0..48)
                    .map(|column| {
                        let x = width * (8 * 48 + column * 70) / (100 * 48);
                        pair[0].luma(x, y).abs_diff(pair[1].luma(x, y)) as f64
                    })
                    .sum::<f64>()
                    / 48.0
            })
            .collect();
        // 只看画面中部（1/3~5/6）：中部没有明显变化说明这两帧没有滚动，
        // 其差异曲线（比如纯噪声）不参与统计。
        let middle = &rows[height / 3..height * 5 / 6];
        if middle.iter().sum::<f64>() / middle.len() as f64 > 1.0 {
            differences.push(rows);
        }
    }
    if differences.len() < 2 {
        return (0.0, 0.0);
    }
    // 对每一行取各帧对差异的中位数，滤掉偶发的局部动画，保留稳定趋势。
    let changes: Vec<f64> = (0..height)
        .map(|y| {
            let mut scores: Vec<f64> = differences.iter().map(|d| d[y]).collect();
            scores.sort_unstable_by(f64::total_cmp);
            scores[scores.len() / 2]
        })
        .collect();
    // 用 4 行滑动窗口平滑：窗口均值 > 2.5 的行视为“正在滚动的正文行”。
    let active: Vec<usize> = (0..height.saturating_sub(4))
        .filter(|&y| changes[y..y + 4].iter().sum::<f64>() / 4.0 > 2.5)
        .collect();
    // 在 active 里找最长连续段：正文滚动的行应连成一大片，固定栏行则完全不活跃。
    let mut best = (0, 0);
    let mut start = 0;
    for i in 0..active.len() {
        if i == 0 || active[i] - active[i - 1] > height / 30 {
            // 与上一活跃行间隔超过画面高度 1/30，视为段落的开头。
            start = active[i];
        }
        if active[i] - start > best.1 - best.0 {
            best = (start, active[i] + 4);
        }
    }
    // 最长的滚动段必须覆盖画面 1/4 以上，否则判定识别失败、不裁剪。
    if best.1 - best.0 < height / 4 {
        return (0.0, 0.0);
    }
    // 在估计边缘向正文内收几行以降低固定 UI 混入的概率，但不能保证排除所有动画或悬浮控件。
    // 顶部：滚动段起点之上都算顶部固定栏；若段几乎贴顶（<4 行）则判定没有。
    // 高度封顶 45%，防止把“正文未开始滚动”的整片区域都当成固定栏。
    let top = if best.0 < 4 {
        0
    } else {
        (best.0 + 3).min(height * 45 / 100)
    };
    // 底部：滚动段终点之下都算底部固定栏；段几乎贴底则判定没有，封顶 35%。
    let bottom = if height - best.1 < 4 {
        0
    } else {
        (height - best.1 + 3).min(height * 35 / 100)
    };
    (top as f64 / height as f64, bottom as f64 / height as f64)
}

/// 快速筛出有明显中部变化的相邻帧，作为固定栏探测样本。
/// 入参：before/after:&RgbFrame，相同尺寸且像素有效。
/// 返回值：bool，中部约 1/3 至 5/6 区域每三行、每行 48 列平均亮度差大于 1 时 true，否则 false。
/// 异常：不返回 Result，尺寸不匹配或非法像素访问可 panic。
/// 注意事项：只检测像素变化，视频动画或切屏也可能被选中，不证明变化来自垂直滚动；结果只用于探测选样。
pub(crate) fn probe_has_motion(before: &RgbFrame, after: &RgbFrame) -> bool {
    let width = before.width() as usize;
    let height = before.height() as usize;
    let mut difference = 0_u64;
    let mut count = 0_u64;
    for y in (height / 3..height * 5 / 6).step_by(3) {
        for column in 0..48 {
            let x = width * (8 * 48 + column * 70) / (100 * 48);
            difference += before.luma(x, y).abs_diff(after.luma(x, y)) as u64;
            count += 1;
        }
    }
    difference as f64 / count.max(1) as f64 > 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 16 帧人造画面：顶部 60 行、底部 60 行固定，中部纹理随帧号滚动；
    /// 底部固定栏中 350~364 行带轻微动画（模拟压缩噪声），不应被当成正文。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn animated_footer_is_excluded_from_scroll_region() {
        let frames: Vec<_> = (0..16_usize)
            .map(|i| {
                let mut pixels = Vec::new();
                for y in 0..400 {
                    for x in 0..160 {
                        let value = if y < 60 {
                            20
                        } else if y >= 340 {
                            // 模拟视频压缩噪声和导航栏中的轻微动画。
                            if (350..365).contains(&y) {
                                30 + (i % 3) as u8
                            } else {
                                30
                            }
                        } else {
                            20 + ((x * 3 + (y + i * 13) * 7) % 150) as u8
                        };
                        pixels.extend([value; 3]);
                    }
                }
                RgbFrame::new(160, 400, pixels).unwrap()
            })
            .collect();
        let (top, bottom) = detect_fixed_bands(&frames);
        assert!((60.0..=70.0).contains(&(top * 400.0)));
        assert!((60.0..=70.0).contains(&(bottom * 400.0)));
    }

    /// 三帧完全相同（静止视频）时，识别结果应为 (0,0)，即不裁剪、保留整帧。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stationary_probe_keeps_full_frame() {
        let frame = RgbFrame::new(64, 128, vec![20; 64 * 128 * 3]).unwrap();
        assert_eq!(
            detect_fixed_bands(&[frame.clone(), frame.clone(), frame]),
            (0.0, 0.0)
        );
    }
}

use crate::{Crop, RgbFrame};

const COLUMNS: usize = 96;
const SCALE: usize = 4;

pub(crate) struct Features {
    samples: Vec<u8>,
    coarse: Vec<u8>,
    height: usize,
    background: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Motion {
    pub shift: i32,
    pub error: f64,
    pub texture: f64,
}

impl Features {
    pub fn new(frame: &RgbFrame, crop: Crop) -> Self {
        let width = (frame.width() - crop.left - crop.right) as usize;
        let height = (frame.height() - crop.top - crop.bottom) as usize;
        let mut samples = Vec::with_capacity(height * COLUMNS);
        for y in 0..height {
            for column in 0..COLUMNS {
                // 排除左右边缘的滚动条和固定操作按钮，保留正文和列表序号。
                let x = crop.left as usize + width * (8 * COLUMNS + column * 70) / (100 * COLUMNS);
                samples.push(frame.luma(x, y + crop.top as usize));
            }
        }
        let mut histogram = [0_usize; 256];
        for &value in &samples {
            histogram[value as usize] += 1;
        }
        let mut count = 0;
        let background = histogram
            .iter()
            .position(|n| {
                count += n;
                count >= samples.len() / 2
            })
            .unwrap_or(0) as u8;
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

    pub fn motion_to(&self, next: &Self) -> Motion {
        let zero = self.score(next, 0, false, 2, 2);
        if zero.error < 0.015 {
            return zero;
        }
        let coarse_height = self.height / SCALE;
        let max_shift = (coarse_height * 4 / 5) as i32;
        let mut candidates = Vec::with_capacity((max_shift * 2 + 1) as usize);
        for shift in -max_shift..=max_shift {
            let score = self.score(next, shift, true, 3, 3);
            if score.texture >= 1.0 {
                candidates.push(score);
            }
        }
        candidates.sort_unstable_by(|a, b| a.error.total_cmp(&b.error));
        // 分别细化不同的候选峰，避免列表行距产生的周期性假匹配。
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
        let overlap = height.saturating_sub(shift.unsigned_abs() as usize);
        if overlap < 8 {
            return Motion {
                shift,
                error: 1.0,
                texture: 0.0,
            };
        }
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
                difference += av.abs_diff(bv) as u64;
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

/// 根据发生滚动的帧对识别固定栏；静止视频保留整帧。
pub(crate) fn detect_fixed_bands(frames: &[RgbFrame]) -> (f64, f64) {
    let Some(first) = frames.first() else {
        return (0.0, 0.0);
    };
    let height = first.height() as usize;
    let width = first.width() as usize;
    let mut differences = Vec::new();
    for pair in frames.windows(2) {
        if pair
            .iter()
            .any(|frame| frame.width() != first.width() || frame.height() != first.height())
        {
            continue;
        }
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
        let middle = &rows[height / 3..height * 5 / 6];
        if middle.iter().sum::<f64>() / middle.len() as f64 > 1.0 {
            differences.push(rows);
        }
    }
    if differences.len() < 2 {
        return (0.0, 0.0);
    }
    let changes: Vec<f64> = (0..height)
        .map(|y| {
            let mut scores: Vec<f64> = differences.iter().map(|d| d[y]).collect();
            scores.sort_unstable_by(f64::total_cmp);
            scores[scores.len() / 2]
        })
        .collect();
    let active: Vec<usize> = (0..height.saturating_sub(4))
        .filter(|&y| changes[y..y + 4].iter().sum::<f64>() / 4.0 > 2.5)
        .collect();
    let mut best = (0, 0);
    let mut start = 0;
    for i in 0..active.len() {
        if i == 0 || active[i] - active[i - 1] > height / 30 {
            start = active[i];
        }
        if active[i] - start > best.1 - best.0 {
            best = (start, active[i] + 4);
        }
    }
    if best.1 - best.0 < height / 4 {
        return (0.0, 0.0);
    }
    // 向正文内缩几行，保证滚动条或导航栏不会落入拼接区域。
    let top = if best.0 < 4 {
        0
    } else {
        (best.0 + 3).min(height * 45 / 100)
    };
    let bottom = if height - best.1 < 4 {
        0
    } else {
        (height - best.1 + 3).min(height * 35 / 100)
    };
    (top as f64 / height as f64, bottom as f64 / height as f64)
}

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

    #[test]
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

    #[test]
    fn stationary_probe_keeps_full_frame() {
        let frame = RgbFrame::new(64, 128, vec![20; 64 * 128 * 3]).unwrap();
        assert_eq!(
            detect_fixed_bands(&[frame.clone(), frame.clone(), frame]),
            (0.0, 0.0)
        );
    }
}

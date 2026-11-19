use eframe::egui::{Color32, ecolor::Hsva};
use std::collections::BTreeMap;

const RGB_WHEEL: [[u8; 3]; 6] = [
    [255, 0, 0],
    [255, 255, 0],
    [0, 255, 0],
    [0, 255, 255],
    [0, 0, 255],
    [255, 0, 255],
];
const ANCHOR_ORDER: [usize; 6] = [0, 2, 4, 1, 3, 5];
const BLENDS: [u32; 8] = [0, 128, 64, 192, 32, 160, 96, 224];
const PALETTE_LEN: usize = RGB_WHEEL.len() * BLENDS.len();

fn palette_color(index: usize, label: &str) -> Color32 {
    if index < PALETTE_LEN {
        let blend = BLENDS[index / RGB_WHEEL.len()];
        let from = ANCHOR_ORDER[index % RGB_WHEEL.len()];
        let a = RGB_WHEEL[from];
        let b = RGB_WHEEL[(from + 1) % RGB_WHEEL.len()];
        let mut rgb: [u8; 3] = std::array::from_fn(|channel| {
            let mixed =
                (u32::from(a[channel]) * (256 - blend) + u32::from(b[channel]) * blend) / 256;
            (42 + mixed * 213 / 255) as u8
        });
        // Extra luminance keeps deep blue strokes legible on dark footage.
        rgb[1] += rgb[2].saturating_sub(rgb[0].max(rgb[1])) / 4;
        return Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    }
    let hash = label.bytes().fold(2_166_136_261_u32, |acc, byte| {
        (acc ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    let hue = (hash % 3600) as f32 / 3600.;
    Hsva::new(hue, 0.72 + ((hash >> 12) % 18) as f32 / 100., 0.9, 1.).into()
}

#[derive(Default)]
pub(crate) struct ClassColors {
    colors: BTreeMap<String, Color32>,
    next_color: usize,
}

impl ClassColors {
    pub fn insert(&mut self, label: &str) {
        if label.is_empty() || self.colors.contains_key(label) {
            return;
        }
        let color = palette_color(self.next_color, label);
        self.next_color += 1;
        self.colors.insert(label.into(), color);
    }

    pub fn contains(&self, label: &str) -> bool {
        self.colors.contains_key(label)
    }

    pub fn remove(&mut self, label: &str) {
        self.colors.remove(label);
    }

    pub fn color(&self, label: &str) -> Color32 {
        self.colors.get(label).copied().unwrap_or(Color32::GRAY)
    }

    pub fn labels(&self) -> impl Iterator<Item = &String> {
        self.colors.keys()
    }

    pub fn len(&self) -> usize {
        self.colors.len()
    }
}

impl FromIterator<String> for ClassColors {
    fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
        let mut colors = Self::default();
        for label in iter {
            colors.insert(&label);
        }
        colors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_stay_fixed_as_more_classes_are_discovered() {
        let mut colors = ClassColors::default();
        colors.insert("car");
        let car = colors.color("car");
        for index in 1..PALETTE_LEN {
            colors.insert(&format!("class_{index}"));
        }
        colors.insert("car");
        assert_eq!(colors.color("car"), car);
        assert_eq!(colors.len(), PALETTE_LEN);
        let unique: std::collections::HashSet<_> =
            colors.labels().map(|l| colors.color(l)).collect();
        assert_eq!(unique.len(), PALETTE_LEN);
        colors.insert("extra");
        assert_ne!(colors.color("extra"), Color32::GRAY);
        assert_eq!(colors.color("car"), car);
    }

    #[test]
    fn starts_with_rgb_then_introduces_saturated_mixes() {
        assert_eq!(palette_color(0, "red"), Color32::from_rgb(255, 42, 42));
        assert_eq!(palette_color(1, "green"), Color32::from_rgb(42, 255, 42));
        assert_eq!(palette_color(2, "blue"), Color32::from_rgb(42, 95, 255));
        let orange = palette_color(6, "orange");
        assert_eq!(orange.r(), 255);
        assert!(orange.g() > 100 && orange.g() < 200);
        assert_eq!(orange.b(), 42);
    }
}

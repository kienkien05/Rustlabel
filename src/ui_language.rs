use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Language {
    #[default]
    Vietnamese,
    English,
}

impl Language {
    pub fn text<T>(self, english: T, vietnamese: T) -> T {
        match self {
            Self::English => english,
            Self::Vietnamese => vietnamese,
        }
    }

    pub fn shape(self, kind: &str) -> &str {
        if self == Self::English {
            return kind;
        }
        match kind {
            "rectangle" => "hình chữ nhật",
            "polygon" => "đa giác",
            "circle" => "hình tròn",
            "line" => "đoạn thẳng",
            "linestrip" => "đường gấp khúc",
            "point" => "điểm",
            "shape" => "vùng",
            _ => kind,
        }
    }
}

pub(crate) fn install_ui_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let font = "Noto Sans".to_owned();
    fonts.font_data.insert(
        font.clone(),
        egui::FontData::from_static(include_bytes!("../assets/NotoSans-Regular.ttf")).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, font.clone());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push(font);
    ctx.set_fonts(fonts);
}

pub(crate) fn help_ui(ui: &mut egui::Ui, language: Language) {
    let text = |en, vi| language.text(en, vi);
    egui::ScrollArea::vertical().id_salt("help-content").show(ui, |ui| {
        ui.heading(text("Getting started", "Thao tác nhanh"));
        for (english, vietnamese) in [
            (
                "1. Open a folder or select images from the Open menu. Click a row in Files to load an image.",
                "1. Dùng menu Mở để mở thư mục hoặc chọn ảnh. Bấm một hàng trong Tệp để xem ảnh.",
            ),
            (
                "2. Choose a drawing tool, draw on the image, then pick or type a label.",
                "2. Chọn công cụ, vẽ trên ảnh rồi chọn hoặc nhập nhãn.",
            ),
            (
                "3. Use Select to move a region or drag its handles. Copy labels opens a multi-select list. Paste adds them to the current image.",
                "3. Dùng công cụ Chọn để di chuyển vùng hoặc kéo các điểm điều chỉnh. Chép vùng nhãn mở danh sách cho phép chọn nhiều vùng. Dán để thêm chúng vào ảnh hiện tại.",
            ),
            (
                "4. Changes are saved automatically. Click Refresh in Files to load edits made outside RustLabel.",
                "4. RustLabel tự lưu thay đổi. Bấm Cập nhật trong Tệp để nạp thay đổi từ bên ngoài RustLabel.",
            ),
        ] {
            ui.label(text(english, vietnamese));
        }
        ui.separator();
        let command = |key: &str| {
            let modifier = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
            format!("{modifier}+{key}")
        };
        shortcut_section(ui, "help-tools", text("Tools and drawing", "Công cụ và vẽ"), &[
            ("1".into(), text("Select and move a region", "Chọn và di chuyển vùng")),
            ("2".into(), text("Rectangle", "Hình chữ nhật")),
            ("3".into(), text("Polygon", "Đa giác")),
            ("4".into(), text("Circle", "Hình tròn")),
            ("L / G / O".into(), text("Line / Polyline / Point", "Đoạn thẳng / Đường gấp khúc / Điểm")),
            ("Enter".into(), text("Finish polygon or polyline, confirm label", "Hoàn tất đa giác hoặc đường gấp khúc, xác nhận nhãn")),
            ("Esc".into(), text("Cancel drawing or close the label picker", "Hủy hình đang vẽ hoặc đóng hộp chọn nhãn")),
        ]);
        shortcut_section(ui, "help-image", text("Images and annotations", "Ảnh và vùng nhãn"), &[
            ("A / D, ← / →".into(), text("Previous or next image. In Files, ↑ / ↓ also work.", "Ảnh trước hoặc sau. Trong Tệp cũng dùng được ↑ / ↓.")),
            (command("F"), text("Fit image to the canvas", "Đưa ảnh về vừa khung")),
            (command(text("wheel", "lăn chuột")), text("Zoom around the pointer", "Thu phóng tại vị trí con trỏ")),
            (text("Middle drag", "Kéo chuột giữa").into(), text("Pan the image", "Di chuyển khung nhìn")),
            (command("E"), text("Change the label on the selected region", "Đổi nhãn của vùng đang chọn")),
            (command("C"), text("Choose and copy one or more annotation regions", "Chọn và chép một hoặc nhiều vùng nhãn")),
            (command("V"), text("Paste copied regions onto the current image", "Dán các vùng đã chép vào ảnh hiện tại")),
            ("Delete / Backspace".into(), text("Delete the selected region", "Xóa vùng đang chọn")),
            (command("Z"), text("Undo an annotation edit", "Hoàn tác chỉnh sửa nhãn")),
            (command("Shift+Z"), text("Redo an annotation edit", "Làm lại chỉnh sửa nhãn")),
            (command("S"), text("Save annotation changes", "Lưu chỉnh sửa nhãn")),
        ]);
        shortcut_section(ui, "help-files", text("When Files has focus", "Khi bảng Tệp đang được chọn"), &[
            (text("Drag / Shift+click", "Kéo / Shift+bấm").into(), text("Select a range of images", "Chọn một dải ảnh")),
            (command(text("click", "bấm")), text("Toggle selection of an image", "Thêm hoặc bỏ một ảnh khỏi vùng chọn")),
            (command("A"), text("Select the filtered files", "Chọn tất cả tệp đang hiển thị")),
            (command("X/C/V"), text("Cut, copy, or paste files", "Cắt, sao chép hoặc dán tệp")),
            ("F2 / Delete".into(), text("Rename or delete selected files", "Đổi tên hoặc xóa tệp đã chọn")),
            (command("Z"), text("Undo the last file action", "Hoàn tác thao tác tệp gần nhất")),
        ]);
            ui.add_space(8.);
            ui.collapsing(text("Font license", "Giấy phép phông chữ"), |ui| {
                ui.label(egui::RichText::new(include_str!("../assets/NotoSans-OFL.txt")).small());
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vietnamese_help_renders_its_headings_and_accented_letters() {
        let ctx = egui::Context::default();
        install_ui_fonts(&ctx);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(290., 600.),
                )),
                ..Default::default()
            },
            |ctx| {
                assert!(ctx.fonts(|fonts| fonts.has_glyphs(
                    &egui::FontId::proportional(14.),
                    "Trợ giúp, Tiếng Việt, Đổi nhãn và hoàn tất vùng, Ắ Ằ Ặ Ễ Ữ Ự Ề Ở Ỡ"
                )));
                egui::CentralPanel::default().show(ctx, |ui| help_ui(ui, Language::default()));
            },
        );
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Thao tác nhanh")
        }));
    }
}

fn shortcut_section(ui: &mut egui::Ui, id: &str, title: &str, rows: &[(String, &str)]) {
    ui.add_space(6.);
    ui.heading(title);
    let width = (ui.available_width() - 125.).max(70.);
    egui::Grid::new(id)
        .num_columns(2)
        .striped(true)
        .spacing([8., 5.])
        .show(ui, |ui| {
            for (keys, action) in rows {
                ui.label(egui::RichText::new(keys).strong().small());
                ui.vertical(|ui| {
                    ui.set_max_width(width);
                    ui.add(egui::Label::new(*action).wrap());
                });
                ui.end_row();
            }
        });
}

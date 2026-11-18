use eframe::egui::{self, Color32, ColorImage, Image, TextureHandle};

// Cells follow the two rows in icons/interface.svg. The bundled 4× raster keeps
// the native client independent of an SVG parser at runtime.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Glyph {
    Mic,
    MicOff,
    Phones,
    PhonesOff,
    Controls,
    Voice,
    Channel,
    Lock,
    Members,
    Search,
    Details,
    Brand,
}

pub struct Icons(TextureHandle);

impl Icons {
    pub fn load(ctx: &egui::Context) -> Self {
        let data = eframe::icon_data::from_png_bytes(include_bytes!("../icons/interface.png"))
            .expect("bundled interface atlas");
        let image = ColorImage::from_rgba_unmultiplied(
            [data.width as usize, data.height as usize],
            &data.rgba,
        );
        Self(ctx.load_texture("interface-icons", image, egui::TextureOptions::LINEAR))
    }

    pub fn image(&self, icon: Glyph, size: f32, tint: Color32) -> Image<'static> {
        let index = icon as u8;
        let x = f32::from(index % 6);
        let y = f32::from(index / 6);
        let uv = egui::Rect::from_min_max(
            egui::pos2(x / 6., y / 2.),
            egui::pos2((x + 1.) / 6., (y + 1.) / 2.),
        );
        Image::from_texture((self.0.id(), egui::Vec2::splat(size)))
            .uv(uv)
            .tint(tint)
    }

    pub fn button(&self, ui: &egui::Ui, icon: Glyph, label: &str) -> egui::Button<'static> {
        egui::Button::image_and_text(
            self.image(icon, 16., ui.visuals().text_color()),
            label.to_owned(),
        )
    }
}

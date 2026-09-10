//! Bounded SVG rasterization for native clients.

use resvg::{tiny_skia, usvg};
use std::sync::{Arc, OnceLock};

pub const MAX_SVG_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SVG_EDGE: u32 = 1600;

pub fn render_png(source: &[u8], max_edge: u32) -> Result<Vec<u8>, &'static str> {
    if source.is_empty() || source.len() > MAX_SVG_BYTES || max_edge == 0 || max_edge > MAX_SVG_EDGE
    {
        return Err("svg.invalid_payload");
    }
    let mut options = usvg::Options::default();
    options.fontdb = system_fonts().clone();
    // No resources directory means relative external files are not resolved. Embedded data URLs
    // remain supported, while an agent-authored SVG cannot read unrelated desktop files.
    options.resources_dir = None;
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    let tree = usvg::Tree::from_data(source, &options).map_err(|_| "svg.invalid")?;
    let size = tree.size();
    let scale = (max_edge as f32 / size.width().max(size.height())).min(1.0);
    let width = (size.width() * scale).ceil().max(1.0) as u32;
    let height = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("svg.too_large")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let png = pixmap.encode_png().map_err(|_| "svg.render_failed")?;
    // One response must stay below the protocol's 1 MiB frame cap, including CBOR overhead.
    if png.len() > 900 * 1024 {
        return Err("svg.render_too_large");
    }
    Ok(png)
}

fn system_fonts() -> &'static Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = usvg::fontdb::Database::new();
        fonts.load_system_fonts();
        Arc::new(fonts)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_svg_to_png_without_external_resources() {
        let png = render_png(
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="16"><rect width="32" height="16" fill="#268bd2"/></svg>"##,
            256,
        )
        .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}

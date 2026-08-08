//! Draws the ClipVault app icon and writes it as a 1024×1024 PNG.
//!
//! Kept as source rather than a checked-in binary blob so the icon can be
//! adjusted and regenerated. `scripts/make-icon.sh` turns the output into the
//! .icns the bundle actually ships.
//!
//! Run with: cargo run --example icongen -- <output.png>

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("icongen draws with AppKit and is macOS-only");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{
        NSBezierPath, NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSDeviceRGBColorSpace,
        NSGradient, NSGraphicsContext,
    };
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize};

    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "icon.png".to_string());

    let pixels = 1024;
    let size = pixels as f64;

    // Drawing into an explicit bitmap rather than NSImage::lockFocus: the latter
    // is deprecated, and on a Retina display it silently renders at 2×, so the
    // output size would depend on which machine built the icon.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            pixels,
            pixels,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }
    .expect("could not allocate the bitmap");

    let context =
        NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).expect("no drawing context");
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));

    // macOS icons sit on a rounded square inset from the canvas edge, so the
    // shape reads correctly once the system adds its own shadow.
    let inset = size * 0.055;
    let side = size - inset * 2.0;
    // Big Sur's continuous-corner radius is close to 22.4% of the side.
    let radius = side * 0.224;

    let plate = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        NSRect::new(NSPoint::new(inset, inset), NSSize::new(side, side)),
        radius,
        radius,
    );

    // Deep ink rather than the candy-bright gradient every generated icon uses.
    let top = NSColor::colorWithSRGBRed_green_blue_alpha(0.16, 0.18, 0.28, 1.0);
    let bottom = NSColor::colorWithSRGBRed_green_blue_alpha(0.07, 0.08, 0.13, 1.0);
    let gradient =
        NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &bottom, &top)
            .expect("could not build the gradient");
    gradient.drawInBezierPath_angle(&plate, 90.0);

    // A stack of cards, receding up and to the left. Depth carries the idea of
    // history far better than a single centred clipboard, and the diagonal
    // keeps the composition off-axis instead of dead-centre.
    let card_w = side * 0.50;
    let card_h = side * 0.40;
    let card_x = inset + side * 0.26;
    let card_y = inset + side * 0.22;
    let step = side * 0.075;
    let corner = card_w * 0.10;

    // Back two cards, dimmer and progressively smaller, drawn first.
    for (index, (shade, scale)) in [(0.30_f64, 0.86_f64), (0.52, 0.93)].into_iter().enumerate() {
        let depth = (2 - index) as f64;
        let w = card_w * scale;
        let h = card_h * scale;
        let rect = NSRect::new(
            NSPoint::new(
                card_x + (card_w - w) / 2.0 - step * depth * 0.35,
                card_y + step * depth,
            ),
            NSSize::new(w, h),
        );
        let card = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect,
            corner * scale,
            corner * scale,
        );
        NSColor::colorWithSRGBRed_green_blue_alpha(shade, shade * 1.05, shade * 1.25, 1.0)
            .setFill();
        card.fill();
    }

    // Front card.
    let front = NSRect::new(NSPoint::new(card_x, card_y), NSSize::new(card_w, card_h));
    let card = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(front, corner, corner);
    NSColor::colorWithSRGBRed_green_blue_alpha(0.97, 0.97, 0.99, 1.0).setFill();
    card.fill();

    // Ruled lines. The top one takes the accent colour so the icon has a single
    // point of warmth against all that ink, and reads at small sizes.
    let line_h = card_h * 0.085;
    let line_x = front.origin.x + card_w * 0.14;
    let full = card_w * 0.72;

    for (index, (fraction, accent)) in [(1.0_f64, true), (0.78, false), (0.52, false)]
        .into_iter()
        .enumerate()
    {
        let y = front.origin.y + card_h * 0.66 - (line_h * 2.1) * index as f64;
        let line = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            NSRect::new(
                NSPoint::new(line_x, y),
                NSSize::new(full * fraction, line_h),
            ),
            line_h / 2.0,
            line_h / 2.0,
        );
        if accent {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.98, 0.62, 0.20, 1.0).setFill();
        } else {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.62, 0.66, 0.78, 1.0).setFill();
        }
        line.fill();
    }

    NSGraphicsContext::restoreGraphicsState_class();

    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .expect("PNG encoding failed");

    std::fs::write(&out, png.to_vec()).expect("could not write the PNG");
    println!("wrote {out}");
}

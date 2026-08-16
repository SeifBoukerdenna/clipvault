//! Composites a captured app window onto a presentation canvas for the README.
//!
//! The captured PNG must be a window-only capture (`screencapture -l`), so no
//! desktop or unrelated app content can leak into a published image.
//!
//! Run with:
//!   cargo run --example screenshot -- <window.png> <out.png> <W> <H> "Caption"

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("screenshot composition uses AppKit and is macOS-only");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{
        NSBezierPath, NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSCompositingOperation,
        NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
        NSGradient, NSGradientDrawingOptions, NSGraphicsContext, NSImage, NSKernAttributeName,
        NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSShadow, NSStringDrawing,
        NSTextAlignment,
    };
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (input, output, width, height, caption, anchor_top) = match &args[..] {
        [i, o, w, h, c] => (i, o, w, h, c, false),
        [i, o, w, h, c, mode] => (i, o, w, h, c, mode == "top"),
        _ => {
            eprintln!("usage: screenshot <window.png> <out.png> <W> <H> <caption> [top]");
            std::process::exit(1);
        }
    };
    let (w, h): (f64, f64) = (width.parse().unwrap(), height.parse().unwrap());

    let shot = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(input))
        .expect("could not read the window capture");

    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(),
            w as isize, h as isize, 8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
        )
    }
    .expect("could not allocate the canvas");

    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).expect("no context");
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));

    // Background: a diagonal multi-stop gradient rather than a flat wash, so the
    // frame has depth before anything is drawn on it.
    let canvas = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
    let stops = NSArray::from_retained_slice(&[
        NSColor::colorWithSRGBRed_green_blue_alpha(0.04, 0.04, 0.09, 1.0),
        NSColor::colorWithSRGBRed_green_blue_alpha(0.13, 0.11, 0.30, 1.0),
        NSColor::colorWithSRGBRed_green_blue_alpha(0.21, 0.22, 0.46, 1.0),
    ]);
    let gradient = NSGradient::initWithColors(NSGradient::alloc(), &stops).expect("gradient");
    gradient.drawInBezierPath_angle(&NSBezierPath::bezierPathWithRect(canvas), 68.0);

    let native = shot.size();
    let portrait = native.height > native.width * 1.15;

    // A soft radial bloom where the window lands. This is what stops the
    // background reading as a flat rectangle.
    let bloom_center = if portrait {
        NSPoint::new(w * 0.72, h * 0.52)
    } else {
        NSPoint::new(w * 0.5, h * 0.42)
    };
    let bloom_colors = NSArray::from_retained_slice(&[
        NSColor::colorWithSRGBRed_green_blue_alpha(0.38, 0.45, 0.98, 0.45),
        NSColor::colorWithSRGBRed_green_blue_alpha(0.38, 0.45, 0.98, 0.0),
    ]);
    let bloom = NSGradient::initWithColors(NSGradient::alloc(), &bloom_colors).expect("bloom");
    bloom.drawFromCenter_radius_toCenter_radius_options(
        bloom_center,
        0.0,
        bloom_center,
        w * 0.44,
        NSGradientDrawingOptions::empty(),
    );

    // Caption is "headline|subheadline"; the second half is optional.
    let (headline, subhead) = match caption.split_once('|') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (caption.as_str(), ""),
    };

    // Portrait sets the type beside the window; landscape puts it above.
    let (text_x, text_w, align) = if portrait {
        (w * 0.075, w * 0.40, 0isize)
    } else {
        (w * 0.06, w * 0.88, 1)
    };

    let style = NSMutableParagraphStyle::new();
    style.setAlignment(NSTextAlignment(align));
    style.setLineHeightMultiple(0.95);

    // Accent rule, picking up the amber from the app icon.
    let rule_y = if portrait { h * 0.645 } else { h * 0.905 };
    let rule_x = if portrait {
        text_x
    } else {
        (w - h * 0.075) / 2.0
    };
    NSColor::colorWithSRGBRed_green_blue_alpha(0.98, 0.62, 0.20, 1.0).setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        NSRect::new(
            NSPoint::new(rule_x, rule_y),
            NSSize::new(h * 0.075, h * 0.0055),
        ),
        h * 0.003,
        h * 0.003,
    )
    .fill();

    NSGraphicsContext::saveGraphicsState_class();
    let text_shadow = NSShadow::new();
    text_shadow.setShadowOffset(NSSize::new(0.0, -h * 0.003));
    text_shadow.setShadowBlurRadius(h * 0.020);
    text_shadow.setShadowColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.0, 0.0, 0.0, 0.65,
    )));
    text_shadow.set();

    // Heavy and tightly tracked: the default regular face at a default track
    // reads as defaulted rather than designed once scaled to a thumbnail.
    let head_font = NSFont::systemFontOfSize_weight(h * 0.070, 0.56);
    let head_attrs = NSDictionary::from_slices(
        &[
            unsafe { NSFontAttributeName },
            unsafe { NSForegroundColorAttributeName },
            unsafe { NSParagraphStyleAttributeName },
            unsafe { NSKernAttributeName },
        ],
        &[
            &*head_font as &objc2::runtime::AnyObject,
            &*NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 1.0),
            &*style,
            &*NSNumber::new_f64(-h * 0.0018),
        ],
    );
    // Text draws from the TOP of its rect downward, so the rect's top edge is
    // what has to clear the accent rule and stay on canvas.
    let head_rect = if portrait {
        NSRect::new(
            NSPoint::new(text_x, h * 0.40),
            NSSize::new(text_w, h * 0.23),
        )
    } else {
        NSRect::new(
            NSPoint::new(text_x, h * 0.775),
            NSSize::new(text_w, h * 0.095),
        )
    };
    unsafe { NSString::from_str(headline).drawInRect_withAttributes(head_rect, Some(&head_attrs)) };

    if !subhead.is_empty() {
        let sub_font = NSFont::systemFontOfSize_weight(h * 0.031, 0.0);
        let sub_attrs = NSDictionary::from_slices(
            &[
                unsafe { NSFontAttributeName },
                unsafe { NSForegroundColorAttributeName },
                unsafe { NSParagraphStyleAttributeName },
            ],
            &[
                &*sub_font as &objc2::runtime::AnyObject,
                &*NSColor::colorWithSRGBRed_green_blue_alpha(0.74, 0.78, 0.94, 1.0),
                &*style,
            ],
        );
        let sub_rect = if portrait {
            NSRect::new(
                NSPoint::new(text_x, h * 0.295),
                NSSize::new(text_w, h * 0.09),
            )
        } else {
            NSRect::new(
                NSPoint::new(text_x, h * 0.700),
                NSSize::new(text_w, h * 0.055),
            )
        };
        unsafe {
            NSString::from_str(subhead).drawInRect_withAttributes(sub_rect, Some(&sub_attrs))
        };
    }
    NSGraphicsContext::restoreGraphicsState_class();

    // Fill the frame: a window floating in a sea of background reads as an
    // afterthought.
    let scale = if portrait {
        ((w * 0.40) / native.width).min((h * 0.86) / native.height)
    } else {
        ((w * 0.88) / native.width).min((h * 0.60) / native.height)
    };
    let draw = NSSize::new(native.width * scale, native.height * scale);
    let rect = if portrait {
        NSRect::new(NSPoint::new(w * 0.55, h * 0.5 - draw.height / 2.0), draw)
    } else if anchor_top {
        // For animation frames: the palette shrinks to fit its results, and
        // centring each frame would make the window jump between them. The app
        // itself pins the top edge, so match that.
        NSRect::new(
            NSPoint::new((w - draw.width) / 2.0, h * 0.64 - draw.height),
            draw,
        )
    } else {
        NSRect::new(
            NSPoint::new((w - draw.width) / 2.0, h * 0.34 - draw.height / 2.0),
            draw,
        )
    };

    // A soft shadow so the window sits on the background rather than looking
    // pasted onto it.
    NSGraphicsContext::saveGraphicsState_class();
    let shadow = NSShadow::new();
    shadow.setShadowOffset(NSSize::new(0.0, -h * 0.012));
    shadow.setShadowBlurRadius(h * 0.030);
    shadow.setShadowColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.0, 0.0, 0.0, 0.55,
    )));
    shadow.set();

    // A zero source rect means "the whole image".
    shot.drawInRect_fromRect_operation_fraction(
        rect,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
        NSCompositingOperation::SourceOver,
        1.0,
    );
    NSGraphicsContext::restoreGraphicsState_class();

    NSGraphicsContext::restoreGraphicsState_class();

    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .expect("PNG encoding failed");
    std::fs::write(output, png.to_vec()).expect("could not write the screenshot");
    println!("wrote {output}  ({}x{})", w as i64, h as i64);
}

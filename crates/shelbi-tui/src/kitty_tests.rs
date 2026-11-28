use super::*;

use ratatui::buffer::Buffer;
use ratatui::widgets::{Clear, Widget};

/// A `w`×`h` PNG of noise at `dir/name` (noise, so its pixel data runs long).
fn png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    let path = dir.join(name);
    let mut seed = 0x2545_f491_u32;
    image::RgbaImage::from_fn(w, h, |_, _| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let [a, b, c, _] = seed.to_le_bytes();
        image::Rgba([a, b, c, 255])
    })
    .save(&path)
    .expect("write a test png");
    path
}

fn gfx() -> Graphics {
    Graphics { cell: (10, 20) }
}

#[test]
fn an_image_keeps_its_size_where_it_fits_and_scales_down_to_the_column() {
    // 200×100 px in 10×20 cells: 20 cols, 5 rows, under the column's 80.
    assert_eq!(fit((200, 100), (10, 20), 80, MAX_ROWS), Some((20, 5)));
    // Wider than the column: scaled to its 60 cols, aspect kept (1600×400
    // px → 600×150 → 60 cols, 7.5 → 8 rows).
    assert_eq!(fit((1600, 400), (10, 20), 60, MAX_ROWS), Some((60, 8)));
    // Tall: capped at 20 rows (400 px), its width following (400×1600 →
    // 100×400 → 10 cols).
    assert_eq!(fit((400, 1600), (10, 20), 80, MAX_ROWS), Some((10, 20)));
    // Tiny images still take a cell.
    assert_eq!(fit((1, 1), (10, 20), 80, MAX_ROWS), Some((1, 1)));
    // Nothing to draw, or no room.
    assert_eq!(fit((0, 10), (10, 20), 80, MAX_ROWS), None);
    assert_eq!(fit((10, 10), (10, 20), 0, MAX_ROWS), None);
}

#[test]
fn the_cell_size_comes_from_the_window_pixels_when_reported() {
    assert_eq!(cell_size(100, 40, 1000, 800), Some((10, 20)));
    assert_eq!(cell_size(100, 40, 0, 0), None, "a terminal that leaves the pixels out");
    assert_eq!(cell_size(0, 0, 1000, 800), None);
}

#[test]
fn a_figure_shows_the_rows_of_it_inside_the_viewport() {
    // A 5-row figure at line 10, in a 10-row viewport.
    assert_eq!(visible(10, 5, 0, 20), Some((10, 0, 5)), "all of it");
    assert_eq!(visible(10, 5, 12, 10), Some((0, 2, 3)), "its top two scrolled off");
    assert_eq!(visible(10, 5, 2, 10), Some((8, 0, 2)), "its bottom three below the fold");
    assert_eq!(visible(10, 5, 15, 10), None, "scrolled past");
    assert_eq!(visible(10, 5, 0, 10), None, "not reached yet");
}

#[test]
fn figures_lay_out_rows_and_a_caption_or_leave_the_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let path = png(dir.path(), "shot.png", 300, 100);
    let (lines, fig) = figure("Board after the fix", &ImageSource::File(path.clone()), 80, gfx()).expect("drawable");
    assert_eq!((fig.cols, fig.rows, fig.line), (30, 5, 0));
    assert_eq!(fig.path, path);
    assert_eq!(lines.len(), 6, "five picture rows, then the caption");
    assert!(lines[..5].iter().all(|l| l.width() == 0));
    let caption: String = lines[5].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(caption, "Board after the fix");
    // No alt text: no caption.
    assert_eq!(figure("", &ImageSource::File(path), 80, gfx()).unwrap().0.len(), 5);
    // Missing files, remote windows, web images and non-images keep the
    // placeholder.
    let gone = dir.path().join("gone.png");
    assert!(figure("a", &ImageSource::Missing(gone.clone()), 80, gfx()).is_none());
    assert!(figure("a", &ImageSource::File(gone), 80, gfx()).is_none());
    assert!(figure("a", &ImageSource::Unavailable, 80, gfx()).is_none());
    assert!(figure("a", &ImageSource::Web("https://x.dev/a.png".into()), 80, gfx()).is_none());
    let text = dir.path().join("notes.png");
    std::fs::write(&text, "not an image").unwrap();
    assert!(figure("a", &ImageSource::File(text), 80, gfx()).is_none());
}

#[test]
fn a_picture_keeps_only_the_rows_nothing_was_drawn_over() {
    let area = Rect::new(0, 0, 20, 10);
    let p = Placement { path: "a.png".into(), x: 2, y: 1, cols: 6, rows: 8, skip: 0, shown: 8 };
    let fresh = || {
        let mut buf = Buffer::empty(area);
        reserve(&mut buf, p.rect());
        buf
    };
    // Nothing over it.
    assert_eq!(clip_to_intact(&fresh(), vec![p.clone()]), vec![p.clone()]);
    // The status band over its last row, a toast over its first two.
    let mut buf = fresh();
    buf.set_string(0, 8, "status", Style::default());
    buf.set_string(5, 1, "toast", Style::default());
    buf.set_string(5, 2, "toast", Style::default());
    let kept = clip_to_intact(&buf, vec![p.clone()]);
    assert_eq!(kept, vec![Placement { y: 3, skip: 2, shown: 5, ..p.clone() }]);
    // An overlay's dimmed backdrop over all of it, and a cleared popup.
    let mut buf = fresh();
    crate::shell::dim_area(&mut buf, area);
    assert!(clip_to_intact(&buf, vec![p.clone()]).is_empty());
    let mut buf = fresh();
    Clear.render(Rect::new(0, 0, 20, 10), &mut buf);
    assert!(clip_to_intact(&buf, vec![p]).is_empty());
}

#[test]
fn base64_pads_like_the_standard() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

/// The tracker's whole life, in one test (the wipe is process-wide).
#[test]
fn images_are_sent_once_placed_by_id_moved_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = png(dir.path(), "a.png", 300, 100);
    let mut images = Images::inline_for_tests();
    images.enable(gfx());
    let at = |y: u16, skip: u16, shown: u16| Placement { path: path.clone(), x: 4, y, cols: 30, rows: 5, skip, shown };

    // The first frame starts the decode and draws nothing yet.
    assert!(images.frame(&[at(3, 0, 5)]).is_empty());
    assert_eq!(images.poll(), (true, false), "the decode landed");

    // Then it's sent once (PNG, chunked, quiet) and placed at its cell,
    // the cursor saved and restored around it.
    let out = text(&images.frame(&[at(3, 0, 5)]));
    let id = ID_BASE;
    assert!(out.starts_with(&format!("\x1b_Ga=t,t=d,f=100,i={id},q=2,m=")), "{out:.80}");
    assert_eq!(out.matches("a=t").count(), 1);
    assert!(out.contains(&format!("\x1b7\x1b[4;5H\x1b_Ga=p,i={id},p=1,c=30,r=5,x=0,y=0,w=300,h=100,C=1,q=2\x1b\\\x1b8")), "{out}");

    // Unchanged: nothing to write.
    assert!(images.frame(&[at(3, 0, 5)]).is_empty());

    // Scrolled so its top two rows are cut: moved, cropped by its source
    // rectangle, never sent again.
    let out = text(&images.frame(&[at(0, 2, 3)]));
    assert!(!out.contains("a=t"));
    assert!(out.contains(&format!("a=p,i={id},p=1,c=30,r=3,x=0,y=40,w=300,h=60,")), "{out}");

    // The same picture twice on screen: a second placement of the one image.
    let out = text(&images.frame(&[at(0, 2, 3), at(10, 0, 5)]));
    assert!(out.contains(&format!("a=p,i={id},p=2,")), "{out}");

    // Off screen (scrolled away, another page): deleted, both of them.
    let out = text(&images.frame(&[]));
    assert_eq!(out, format!("\x1b_Ga=d,d=i,i={id},p=1,q=2\x1b\\\x1b_Ga=d,d=i,i={id},p=2,q=2\x1b\\"));
    assert!(images.frame(&[]).is_empty());

    // Handing the terminal away wipes everything; back, it's sent again.
    images.frame(&[at(3, 0, 5)]);
    let mut wiped = Vec::new();
    wipe(&mut wiped);
    assert_eq!(wiped, DELETE_ALL);
    let out = text(&images.frame(&[at(3, 0, 5)]));
    assert_eq!(out.matches("a=t").count(), 1, "sent again after the wipe");
    assert!(out.contains("a=p,"));

    // A full repaint places it again without sending it.
    images.forget_placements();
    let out = text(&images.frame(&[at(3, 0, 5)]));
    assert!(!out.contains("a=t") && out.contains("a=p,"), "{out:.80}");
}

#[test]
fn an_image_that_wont_decode_falls_back_to_its_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    // A PNG header (so its size reads) over a truncated body.
    let good = png(dir.path(), "full.png", 64, 64);
    let bytes = std::fs::read(&good).unwrap();
    let broken = dir.path().join("broken.png");
    std::fs::write(&broken, &bytes[..bytes.len() / 2]).unwrap();
    assert!(dims(&broken).is_some(), "the header reads");
    let mut images = Images::inline_for_tests();
    images.enable(gfx());
    let p = Placement { path: broken.clone(), x: 0, y: 0, cols: 4, rows: 2, skip: 0, shown: 2 };
    assert!(images.frame(std::slice::from_ref(&p)).is_empty());
    assert_eq!(images.poll(), (true, true), "landed, as a failure");
    assert!(images.frame(&[p]).is_empty(), "never sent");
    assert!(figure("a", &ImageSource::File(broken), 80, gfx()).is_none(), "laid out again as a placeholder");
}

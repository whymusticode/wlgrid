# Plan: unit_render.rs - Wayland Layer-Shell Rendering Test

## Goal

Make `unit_render.rs` a proper test of the rendering and interaction model that `main.rs` needs, but using native Wayland layer-shell instead of eframe/winit. This proves the approach works on Wayland before migrating the full app.

---

## Phase 1: Icon/Image Rendering

### 1.1 Add image blitting to SHM buffer

Currently `draw()` fills a gradient. Need to:

```rust
// Add to App struct:
icon_cache: HashMap<String, Vec<u8>>,  // icon_name -> RGBA pixels
icon_size: (u32, u32),                  // cached icon dimensions
```

```rust
// New function to load an icon into RGBA buffer:
fn load_icon_rgba(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_vec(), w, h))
}
```

```rust
// New function to blit RGBA onto canvas at position:
fn blit_rgba(
    canvas: &mut [u8],       // destination ARGB8888 buffer
    canvas_w: u32,           // destination width
    src: &[u8],              // source RGBA buffer
    src_w: u32, src_h: u32,  // source dimensions
    dst_x: i32, dst_y: i32,  // destination position
) {
    for sy in 0..src_h {
        for sx in 0..src_w {
            let dx = dst_x + sx as i32;
            let dy = dst_y + sy as i32;
            if dx < 0 || dy < 0 || dx >= canvas_w as i32 { continue; }

            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy as u32 * canvas_w + dx as u32) * 4) as usize;

            // RGBA -> ARGB (Wayland expects ARGB8888)
            let (r, g, b, a) = (src[src_idx], src[src_idx+1], src[src_idx+2], src[src_idx+3]);
            canvas[dst_idx..dst_idx+4].copy_from_slice(&[b, g, r, a]); // BGRA little-endian = ARGB
        }
    }
}
```

### 1.2 Test with hardcoded icon

For initial testing, load a single known icon:

```rust
// In main() or App::new():
let test_icon_path = PathBuf::from("/usr/share/icons/hicolor/96x96/apps/firefox.png");
let test_icon = load_icon_rgba(&test_icon_path);
```

Draw it centered in `draw()`:

```rust
if let Some((ref pixels, iw, ih)) = self.test_icon {
    let x = (self.width - iw) as i32 / 2;
    let y = (self.height - ih) as i32 / 2;
    blit_rgba(canvas, self.width, pixels, iw, ih, x, y);
}
```

---

## Phase 2: Grid Layout

### 2.1 Add grid configuration

```rust
// Add to App struct:
grid_w: usize,          // columns (e.g., 6)
grid_h: usize,          // rows (e.g., 4)
tile_size: u32,         // pixels per tile (e.g., 96)
tile_gap: u32,          // gap between tiles (e.g., 8)
tiles: Vec<Option<TestEntry>>,  // what's in each slot

struct TestEntry {
    name: String,
    icon_rgba: Vec<u8>,
    icon_w: u32,
    icon_h: u32,
}
```

### 2.2 Tile rect calculation

```rust
fn tile_rect(&self, index: usize) -> (i32, i32, u32, u32) {
    let col = index % self.grid_w;
    let row = index / self.grid_w;
    let x = 8 + (col as u32 * (self.tile_size + self.tile_gap)) as i32;
    let y = 8 + (row as u32 * (self.tile_size + self.tile_gap)) as i32;
    (x, y, self.tile_size, self.tile_size)
}
```

### 2.3 Draw grid

```rust
fn draw(&mut self, qh: &QueueHandle<Self>) {
    // ... create buffer ...

    // Clear to dark background
    canvas.chunks_exact_mut(4).for_each(|c| {
        c.copy_from_slice(&[0x12, 0x12, 0x12, 0xFF]); // dark gray BGRA
    });

    // Draw each tile
    for i in 0..(self.grid_w * self.grid_h) {
        let (x, y, w, h) = self.tile_rect(i);

        // Draw tile background
        fill_rect(canvas, self.width, x, y, w, h, [0x2D, 0x2D, 0x2D, 0xFF]);

        // Draw icon if present
        if let Some(ref entry) = self.tiles[i] {
            let ix = x + (w as i32 - entry.icon_w as i32) / 2;
            let iy = y + (h as i32 - entry.icon_h as i32) / 2;
            blit_rgba(canvas, self.width, &entry.icon_rgba, entry.icon_w, entry.icon_h, ix, iy);
        }
    }

    // ... commit buffer ...
}

fn fill_rect(canvas: &mut [u8], canvas_w: u32, x: i32, y: i32, w: u32, h: u32, color: [u8; 4]) {
    for dy in 0..h {
        for dx in 0..w {
            let px = x + dx as i32;
            let py = y + dy as i32;
            if px < 0 || py < 0 || px >= canvas_w as i32 { continue; }
            let idx = ((py as u32 * canvas_w + px as u32) * 4) as usize;
            canvas[idx..idx+4].copy_from_slice(&color);
        }
    }
}
```

---

## Phase 3: Pointer Interaction

### 3.1 Track pointer state

```rust
// Add to App struct:
pointer_pos: (f64, f64),         // current pointer position
press_start: Option<(f64, f64, usize)>,  // (x, y, tile_index) when pressed
hovered_tile: Option<usize>,     // which tile is under pointer
```

### 3.2 Hit testing

```rust
fn tile_at(&self, x: f64, y: f64) -> Option<usize> {
    for i in 0..(self.grid_w * self.grid_h) {
        let (tx, ty, tw, th) = self.tile_rect(i);
        if x >= tx as f64 && x < (tx + tw as i32) as f64 &&
           y >= ty as f64 && y < (ty + th as i32) as f64 {
            return Some(i);
        }
    }
    None
}
```

### 3.3 Update PointerHandler

```rust
impl PointerHandler for App {
    fn pointer_frame(&mut self, conn: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let mut needs_redraw = false;

        for ev in events {
            if ev.surface != *self.layer.wl_surface() { continue; }

            match ev.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_pos = ev.position;
                    let new_hovered = self.tile_at(ev.position.0, ev.position.1);
                    if new_hovered != self.hovered_tile {
                        self.hovered_tile = new_hovered;
                        needs_redraw = true;
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.hovered_tile = None;
                    needs_redraw = true;
                }
                PointerEventKind::Press { button, .. } => {
                    if button == 0x110 { // BTN_LEFT
                        if let Some(tile) = self.tile_at(ev.position.0, ev.position.1) {
                            self.press_start = Some((ev.position.0, ev.position.1, tile));
                        }
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    // Handle in Phase 4
                }
                _ => {}
            }
        }

        if needs_redraw {
            self.draw(qh);
        }
    }
}
```

### 3.4 Visual feedback for hover

In `draw()`, modify tile background based on hover:

```rust
let bg_color = if self.hovered_tile == Some(i) {
    [0x46, 0x46, 0x46, 0xFF]  // lighter when hovered
} else {
    [0x2D, 0x2D, 0x2D, 0xFF]  // default
};
fill_rect(canvas, self.width, x, y, w, h, bg_color);
```

---

## Phase 4: Drag and Drop

### 4.1 Track drag state

```rust
// Add to App struct:
drag_from: Option<usize>,        // tile being dragged (None = not dragging)
drag_threshold: f64,             // pixels before press becomes drag (e.g., 5.0)
```

### 4.2 Detect drag vs click

In `PointerEventKind::Motion` handling:

```rust
if let Some((px, py, tile)) = self.press_start {
    let dx = ev.position.0 - px;
    let dy = ev.position.1 - py;
    let dist = (dx*dx + dy*dy).sqrt();

    if dist > self.drag_threshold && self.drag_from.is_none() {
        // Start dragging
        self.drag_from = Some(tile);
        self.press_start = None;  // no longer a potential click
        needs_redraw = true;
    }
}
```

### 4.3 Handle release

```rust
PointerEventKind::Release { button, .. } => {
    if button == 0x110 { // BTN_LEFT
        if let Some(from) = self.drag_from.take() {
            // Was dragging - do the swap
            if let Some(to) = self.tile_at(ev.position.0, ev.position.1) {
                if from != to {
                    self.tiles.swap(from, to);
                    eprintln!("swapped tile {} <-> {}", from, to);
                }
            }
            needs_redraw = true;
        } else if let Some((_, _, tile)) = self.press_start.take() {
            // Was a click (no drag happened)
            eprintln!("clicked tile {}", tile);
            // TODO: launch entry or open picker
        }
    } else if button == 0x111 { // BTN_RIGHT
        if let Some(tile) = self.tile_at(ev.position.0, ev.position.1) {
            self.tiles[tile] = None;  // remove entry
            eprintln!("removed tile {}", tile);
            needs_redraw = true;
        }
    }
    self.press_start = None;
}
```

### 4.4 Visual feedback during drag

In `draw()`:

```rust
// Draw drop target highlight
if let Some(from) = self.drag_from {
    if let Some(to) = self.hovered_tile {
        if from != to {
            let (x, y, w, h) = self.tile_rect(to);
            // Draw highlight border
            draw_rect_outline(canvas, self.width, x, y, w, h, [0x00, 0xFF, 0x00, 0xFF], 2);
        }
    }
}

// Draw dragged icon following cursor
if let Some(from) = self.drag_from {
    if let Some(ref entry) = self.tiles[from] {
        let x = self.pointer_pos.0 as i32 - entry.icon_w as i32 / 2;
        let y = self.pointer_pos.1 as i32 - entry.icon_h as i32 / 2;
        blit_rgba(canvas, self.width, &entry.icon_rgba, entry.icon_w, entry.icon_h, x, y);
    }
}
```

```rust
fn draw_rect_outline(canvas: &mut [u8], canvas_w: u32, x: i32, y: i32, w: u32, h: u32, color: [u8; 4], thickness: u32) {
    // Top edge
    fill_rect(canvas, canvas_w, x, y, w, thickness, color);
    // Bottom edge
    fill_rect(canvas, canvas_w, x, y + h as i32 - thickness as i32, w, thickness, color);
    // Left edge
    fill_rect(canvas, canvas_w, x, y, thickness, h, color);
    // Right edge
    fill_rect(canvas, canvas_w, x + w as i32 - thickness as i32, y, thickness, h, color);
}
```

---

## Phase 5: Dynamic Surface Sizing

### 5.1 Calculate required size from grid

```rust
fn required_size(&self) -> (u32, u32) {
    let w = 16 + self.grid_w as u32 * self.tile_size + (self.grid_w.saturating_sub(1) as u32 * self.tile_gap);
    let h = 16 + self.grid_h as u32 * self.tile_size + (self.grid_h.saturating_sub(1) as u32 * self.tile_gap);
    (w, h)
}
```

### 5.2 Set layer surface size in main()

```rust
let (req_w, req_h) = app.required_size();
layer.set_size(req_w, req_h);
```

### 5.3 Resize pool when surface is configured

In `LayerShellHandler::configure`:

```rust
if self.width != old_w || self.height != old_h {
    let new_size = (self.width * self.height * 4) as usize;
    if self.pool.len() < new_size {
        self.pool.resize(new_size).expect("pool resize");
    }
}
```

---

## Summary: Files to Change

1. **src/unit_render.rs** - all the above changes

2. **Cargo.toml** - update bin path after rename:
   ```toml
   [[bin]]
   name = "wlgrid-layer"
   path = "src/unit_render.rs"
   ```

---

## Implementation Order

1. Phase 1.1-1.2: Get one icon rendering (proves blitting works)
2. Phase 2: Grid layout with multiple tiles
3. Phase 3: Hover detection and visual feedback
4. Phase 4: Drag and drop
5. Phase 5: Dynamic sizing

Each phase can be tested independently. Phase 1 is the critical proof that the approach works.

---

## Not Included (Future Work)

- Text rendering (needs fontdue or similar)
- Picker window (second layer surface or inline state)
- Config file loading (reuse from main.rs later)
- Entry launching (reuse run_shell from main.rs later)
- Icon index building (reuse from main.rs later)
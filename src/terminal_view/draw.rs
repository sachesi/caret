use super::*;

/// How long a frame rasterises glyphs itself before it leaves the rest to the glyph thread.
const GLYPH_TIME: Duration = Duration::from_millis(8);

fn rgba(color: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        1.0,
    )
}

impl TerminalView {
    /// Remakes the fonts when the font, the scale or the resolution changed.
    pub(super) fn ensure_fonts(&self) {
        let imp = self.imp();
        let scale = self.scale();
        let gtk_settings = gtk::Settings::default();
        let dpi = gtk_settings
            .as_ref()
            .map(gtk::Settings::gtk_xft_dpi)
            .filter(|&dpi| dpi > 0)
            .unwrap_or(96 * 1024);
        let mut font = settings::font();
        let factor = ZOOM_STEP.powi(imp.zoom.get());
        if font.is_size_absolute() {
            font.set_absolute_size(f64::from(font.size()) * factor * scale);
        } else {
            font.set_size((f64::from(font.size()) * factor).round() as i32);
        }
        let features = settings().string("font-features");
        let made_for = (format!("{font} {features}"), scale.to_bits(), dpi);
        if imp.fonts_made_for.borrow().as_ref() == Some(&made_for) {
            return;
        }
        let Ok(mut options) = cairo::FontOptions::new() else {
            return;
        };
        // Grey antialiasing: glyphs are masks tinted per cell, which subpixel colour
        // fringes would not survive.
        options.set_antialias(cairo::Antialias::Gray);
        options.set_hint_metrics(cairo::HintMetrics::On);
        let hinting = gtk_settings
            .as_ref()
            .and_then(gtk::Settings::gtk_xft_hintstyle);
        options.set_hint_style(match hinting.as_deref() {
            Some("hintnone") => cairo::HintStyle::None,
            Some("hintslight") => cairo::HintStyle::Slight,
            Some("hintmedium") => cairo::HintStyle::Medium,
            Some("hintfull") => cairo::HintStyle::Full,
            _ => cairo::HintStyle::Default,
        });
        let resolution = f64::from(dpi) / 1024.0 * scale;
        imp.fonts
            .replace(Some(Fonts::new(&font, resolution, &options, &features)));
        imp.glyph_thread.take();
        imp.finished_glyphs.borrow_mut().clear();
        imp.fonts_made_for.replace(Some(made_for));
        imp.glyphs_stale.set(true);
    }

    /// Has the glyph thread rasterise `key`, starting it the first time, and draws again
    /// as glyphs come back; false when the thread cannot be started.
    fn request_glyph(&self, fonts: &Fonts, key: &GlyphKey) -> bool {
        let mut thread = self.imp().glyph_thread.borrow_mut();
        if thread.is_none() {
            let (spawned, results) = match GlyphThread::spawn(fonts) {
                Ok(spawned) => spawned,
                Err(err) => {
                    tracing::warn!("starting the glyph thread: {err}");
                    return false;
                }
            };
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = view)]
                self,
                async move {
                    while let Ok((key, bitmap)) = results.recv().await {
                        // Closed when the fonts changed: what is left is in the old ones.
                        if results.is_closed() {
                            break;
                        }
                        let imp = view.imp();
                        if let Some(thread) = imp.glyph_thread.borrow_mut().as_mut() {
                            thread.received(&key);
                        }
                        imp.finished_glyphs.borrow_mut().insert(key, bitmap);
                        view.queue_draw();
                    }
                }
            ));
            *thread = Some(spawned);
        }
        if let Some(thread) = thread.as_mut() {
            thread.request(key);
        }
        true
    }

    fn glyph_pending(&self, key: &GlyphKey) -> bool {
        let thread = self.imp().glyph_thread.borrow();
        thread.as_ref().is_some_and(|thread| thread.is_pending(key))
    }

    #[expect(clippy::too_many_lines, reason = "one frame from capture to texture")]
    pub(super) fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let (width, height) = (self.width(), self.height());
        if width <= 0 || height <= 0 {
            return;
        }
        let palette = imp.palette.get();
        let bounds = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
        if let Some(message) = imp.failure.borrow().as_ref() {
            snapshot.append_color(&rgba(palette.background), &bounds);
            let layout = self.create_pango_layout(Some(message));
            layout.set_width((width - 24).max(1) * pango::SCALE);
            layout.set_wrap(pango::WrapMode::WordChar);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(12.0, 12.0));
            snapshot.append_layout(&layout, &rgba(palette.foreground));
            snapshot.restore();
            return;
        }

        self.ensure_fonts();
        let session = imp.session.borrow();
        let mut gl = imp.gl.borrow_mut();
        let fonts = imp.fonts.borrow();
        let (Some(session), Some((context, renderer)), Some(fonts)) =
            (session.as_ref(), gl.as_mut(), fonts.as_ref())
        else {
            snapshot.append_color(&rgba(palette.background), &bounds);
            return;
        };

        let mut screen = imp.screen.borrow_mut();
        let term = session.lock();
        // Rearmed under the lock: output after the capture wakes the widget again.
        session.rearm();
        // A click on a link goes to a program that reports the mouse, so it is not marked.
        let link = imp
            .hovered_cell
            .get()
            .filter(|_| !term.mode().intersects(TermMode::MOUSE_MODE))
            .and_then(|point| input::link_in(&term, point));
        imp.link_underlined.set(link.is_some());
        screen.capture(&term, &palette, link.as_ref().map(|link| &link.span));
        let scroll = (
            term.history_size(),
            term.screen_lines(),
            term.grid().display_offset(),
        );
        drop(term);
        self.follow_scroll(scroll);
        self.update_accessible_text(&screen);
        let scale = self.scale();
        let look = Look {
            selection: palette.selection,
            focused: imp.focused.get(),
            cursor_on: imp.cursor_on.get() || !screen.cursor_blinks,
            padding: padding(scale),
            scale,
            ligatures: settings().boolean("ligatures"),
        };

        context.make_current();
        if imp.glyphs_stale.replace(false) {
            renderer.clear_glyphs(false);
        }
        let mut quads = imp.quads.borrow_mut();
        let mut finished = imp.finished_glyphs.take();
        let deadline = Instant::now() + GLYPH_TIME;
        let mut rasterize = |key: &GlyphKey| {
            if let Some(bitmap) = finished.remove(key) {
                Some(bitmap)
            } else if self.glyph_pending(key) {
                None
            } else if Instant::now() < deadline || !self.request_glyph(fonts, key) {
                Some(fonts.rasterize(key))
            } else {
                None
            }
        };
        let mut build = |renderer: &mut Renderer, quads: &mut Quads, partial| {
            frame::build(
                &screen,
                &look,
                fonts.cell,
                &|run, style| fonts.ligatures(run, style),
                &mut rasterize,
                renderer,
                quads,
                partial,
            )
        };
        let mut built = build(renderer, &mut quads, false);
        if built.is_err() {
            renderer.clear_glyphs(true);
            built = build(renderer, &mut quads, false);
        }
        if built.is_err() {
            // More glyphs than the largest atlas holds: what fits is shown.
            renderer.clear_glyphs(false);
            let _ = build(renderer, &mut quads, true);
        }

        // The texture starts on the screen pixel at or before the widget's corner, which
        // on a fractional scale can fall between pixels.
        let (x, y) = self.origin_on_screen(scale);
        let (x, y) = (x - x.floor(), y - y.floor());
        // Past what the GPU allows, the rest of the widget is left out.
        let largest = renderer.max_texture_size();
        let pixels_wide = ((x + f64::from(width) * scale).ceil() as i32).min(largest);
        let pixels_high = ((y + f64::from(height) * scale).ceil() as i32).min(largest);
        let background = screen.background;
        match renderer.render(
            context,
            pixels_wide,
            pixels_high,
            [background.r, background.g, background.b],
            &quads,
        ) {
            Ok(texture) => {
                // One texel to one screen pixel on the pixel grid, so sampling copies them.
                let size = graphene::Rect::new(
                    (-x / scale) as f32,
                    (-y / scale) as f32,
                    (f64::from(pixels_wide) / scale) as f32,
                    (f64::from(pixels_high) / scale) as f32,
                );
                snapshot.push_clip(&bounds);
                snapshot.append_texture(&texture, &size);
                snapshot.pop();
            }
            Err(error) => {
                tracing::warn!("drawing a frame: {error}");
                snapshot.append_color(&rgba(background), &bounds);
            }
        }

        if let (Some(cursor), Some(im)) = (screen.cursor, imp.im.get()) {
            let cell = fonts.cell;
            let x = f64::from(look.padding + cursor.column as i32 * cell.width) / scale;
            let y = f64::from(look.padding + cursor.line as i32 * cell.height) / scale;
            im.set_cursor_location(&gdk::Rectangle::new(
                x as i32,
                y as i32,
                (f64::from(cell.width) / scale).ceil() as i32,
                (f64::from(cell.height) / scale).ceil() as i32,
            ));
        }
    }
}

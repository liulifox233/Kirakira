use std::io::{self, Read, Seek};

pub trait ResourceStream: Read + Seek + Send {}

impl<T> ResourceStream for T where T: Read + Seek + Send {}

pub trait ResourceProvider: Send + Sync {
    fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>>;

    fn exists(&self, path: &str) -> bool;
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }

    pub fn inset(self, amount: f32) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - amount * 2.0).max(0.0),
            height: (self.height - amount * 2.0).max(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb_u8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectCommand {
    pub rect: Rect,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawCommand {
    Rect(RectCommand),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameOutput {
    pub clear_color: Color,
    pub clip: Option<Rect>,
    pub draw_commands: Vec<DrawCommand>,
}

impl FrameOutput {
    pub fn new(clear_color: Color, draw_commands: Vec<DrawCommand>) -> Self {
        Self {
            clear_color,
            clip: None,
            draw_commands,
        }
    }

    pub fn with_clip(mut self, clip: Rect) -> Self {
        self.clip = Some(clip);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInput {
    pub viewport_size: Size,
    pub delta_seconds: f32,
}

impl FrameInput {
    pub const fn new(viewport_size: Size, delta_seconds: f32) -> Self {
        Self {
            viewport_size,
            delta_seconds,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    pub initial_viewport: Size,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            initial_viewport: Size::new(960.0, 600.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Other(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineKey {
    Escape,
    Enter,
    Space,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineEvent {
    CursorMoved {
        position: Point,
    },
    PointerInput {
        button: PointerButton,
        state: ButtonState,
    },
    KeyboardInput {
        key: EngineKey,
        state: ButtonState,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Panel {
    Launcher,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiElement {
    Start,
    OpenProject,
    Settings,
    Back,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiAction {
    LaunchRequested,
    OpenProjectRequested,
    SettingsOpened,
    SettingsClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LauncherViewModel {
    pub panel: Panel,
    pub hovered: Option<UiElement>,
    pub pressed: Option<UiElement>,
    pub last_action: Option<UiAction>,
    pub launch_requests: u32,
    pub open_project_requests: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiLayout {
    pub top_bar: Rect,
    pub side_rail: Rect,
    pub settings_nav: Rect,
    pub content: Rect,
    pub hero: Rect,
    pub start_button: Rect,
    pub open_project_button: Rect,
    pub settings_button: Rect,
    pub back_button: Rect,
    panel: Panel,
}

impl UiLayout {
    pub fn new(size: Size, panel: Panel) -> Self {
        let width = size.width.max(320.0);
        let height = size.height.max(360.0);
        let top_h = 56.0;
        let side_w = if width >= 560.0 { 76.0 } else { 0.0 };
        let margin = if width >= 560.0 { 28.0 } else { 16.0 };
        let gap = 16.0;
        let content_x = side_w + margin;
        let content_y = top_h + margin;
        let content_w = (width - content_x - margin).max(0.0);
        let content_h = (height - content_y - margin).max(0.0);
        let hero_h = content_h.mul_add(0.28, 0.0).clamp(92.0, 150.0);
        let hero = Rect::new(content_x, content_y, content_w, hero_h);
        let tile_y = hero.y + hero.height + 28.0;

        let (start_button, settings_button) = if content_w >= 560.0 {
            let start_w = (content_w - gap) * 0.62;
            (
                Rect::new(content_x, tile_y, start_w, 132.0),
                Rect::new(
                    content_x + start_w + gap,
                    tile_y,
                    content_w - start_w - gap,
                    132.0,
                ),
            )
        } else {
            (
                Rect::new(content_x, tile_y, content_w, 112.0),
                Rect::new(content_x, tile_y + 128.0, content_w, 92.0),
            )
        };

        let open_project_button = Rect::new(
            start_button.x + 18.0,
            start_button.y + start_button.height - 42.0,
            (start_button.width - 36.0).max(0.0),
            24.0,
        );
        let settings_nav = Rect::new(18.0, top_h + 22.0, 40.0, 40.0);
        let back_button = Rect::new(content_x, content_y + hero.height + 20.0, 104.0, 42.0);

        Self {
            top_bar: Rect::new(0.0, 0.0, width, top_h),
            side_rail: Rect::new(0.0, top_h, side_w, height - top_h),
            settings_nav,
            content: Rect::new(content_x, content_y, content_w, content_h),
            hero,
            start_button,
            open_project_button,
            settings_button,
            back_button,
            panel,
        }
    }

    pub fn hit_test(&self, point: Point) -> Option<UiElement> {
        if self.panel == Panel::Settings && self.back_button.contains(point) {
            return Some(UiElement::Back);
        }

        if self.panel == Panel::Launcher {
            if self.open_project_button.contains(point) {
                return Some(UiElement::OpenProject);
            }
            if self.start_button.contains(point) {
                return Some(UiElement::Start);
            }
            if self.settings_button.contains(point) || self.settings_nav.contains(point) {
                return Some(UiElement::Settings);
            }
        } else if self.settings_nav.contains(point) {
            return Some(UiElement::Settings);
        }

        None
    }
}

#[derive(Debug)]
pub struct Engine {
    viewport_size: Size,
    cursor_position: Option<Point>,
    panel: Panel,
    hovered: Option<UiElement>,
    pressed: Option<UiElement>,
    last_action: Option<UiAction>,
    status_level: Option<StatusLevel>,
    launch_requests: u32,
    open_project_requests: u32,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            viewport_size: config.initial_viewport,
            cursor_position: None,
            panel: Panel::Launcher,
            hovered: None,
            pressed: None,
            last_action: None,
            status_level: None,
            launch_requests: 0,
            open_project_requests: 0,
        }
    }

    pub fn handle_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::CursorMoved { position } => {
                self.cursor_position = Some(position);
                self.update_hover();
            }
            EngineEvent::PointerInput {
                button: PointerButton::Primary,
                state: ButtonState::Pressed,
            } => {
                self.update_hover();
                self.pressed = self.hovered;
            }
            EngineEvent::PointerInput {
                button: PointerButton::Primary,
                state: ButtonState::Released,
            } => {
                self.update_hover();
                let pressed = self.pressed.take();
                if pressed.is_some() && pressed == self.hovered {
                    self.activate(pressed);
                }
            }
            EngineEvent::KeyboardInput {
                key: EngineKey::Escape,
                state: ButtonState::Pressed,
            } => {
                if self.panel == Panel::Settings {
                    self.panel = Panel::Launcher;
                    self.last_action = Some(UiAction::SettingsClosed);
                    self.update_hover();
                }
            }
            EngineEvent::KeyboardInput {
                key: EngineKey::Enter | EngineKey::Space,
                state: ButtonState::Pressed,
            } => {
                self.activate(self.hovered);
            }
            EngineEvent::PointerInput { .. } | EngineEvent::KeyboardInput { .. } => {}
        }
    }

    pub fn tick(&mut self, input: FrameInput) -> FrameOutput {
        if !input.viewport_size.is_empty() {
            self.viewport_size = input.viewport_size;
        }
        self.update_hover();

        let layout = UiLayout::new(self.viewport_size, self.panel);
        let mut draw_commands = Vec::with_capacity(28);
        self.draw_shell(&mut draw_commands, layout);

        match self.panel {
            Panel::Launcher => self.draw_launcher(&mut draw_commands, layout),
            Panel::Settings => self.draw_settings(&mut draw_commands, layout),
        }

        FrameOutput::new(palette::BACKGROUND, draw_commands)
    }

    pub fn tick_running(&mut self, input: FrameInput) -> FrameOutput {
        if !input.viewport_size.is_empty() {
            self.viewport_size = input.viewport_size;
        }

        let layout = UiLayout::new(self.viewport_size, Panel::Launcher);
        let mut draw_commands = Vec::with_capacity(24);
        self.draw_shell(&mut draw_commands, layout);
        self.draw_running(&mut draw_commands, layout);

        FrameOutput::new(palette::RUNTIME_BACKGROUND, draw_commands)
    }

    pub fn view_model(&self) -> LauncherViewModel {
        LauncherViewModel {
            panel: self.panel,
            hovered: self.hovered,
            pressed: self.pressed,
            last_action: self.last_action,
            launch_requests: self.launch_requests,
            open_project_requests: self.open_project_requests,
        }
    }

    pub fn layout(&self) -> UiLayout {
        UiLayout::new(self.viewport_size, self.panel)
    }

    pub fn panel(&self) -> Panel {
        self.panel
    }

    pub fn set_panel(&mut self, panel: Panel) {
        if self.panel == panel {
            return;
        }

        self.panel = panel;
        self.pressed = None;
        self.update_hover();
    }

    pub fn set_status_level(&mut self, level: Option<StatusLevel>) {
        self.status_level = level;
    }

    pub fn take_last_action(&mut self) -> Option<UiAction> {
        self.last_action.take()
    }

    fn activate(&mut self, element: Option<UiElement>) {
        match element {
            Some(UiElement::Start) => {
                self.launch_requests = self.launch_requests.saturating_add(1);
                self.last_action = Some(UiAction::LaunchRequested);
            }
            Some(UiElement::OpenProject) => {
                self.open_project_requests = self.open_project_requests.saturating_add(1);
                self.last_action = Some(UiAction::OpenProjectRequested);
            }
            Some(UiElement::Settings) => {
                self.panel = Panel::Settings;
                self.last_action = Some(UiAction::SettingsOpened);
                self.pressed = None;
                self.update_hover();
            }
            Some(UiElement::Back) => {
                self.panel = Panel::Launcher;
                self.last_action = Some(UiAction::SettingsClosed);
                self.pressed = None;
                self.update_hover();
            }
            None => {}
        }
    }

    fn update_hover(&mut self) {
        self.hovered = self
            .cursor_position
            .and_then(|point| UiLayout::new(self.viewport_size, self.panel).hit_test(point));
    }

    fn draw_shell(&self, commands: &mut Vec<DrawCommand>, layout: UiLayout) {
        rect(commands, layout.top_bar, palette::TOP_BAR);
        if layout.side_rail.width > 0.0 {
            rect(commands, layout.side_rail, palette::SIDE_RAIL);
            rect(
                commands,
                layout.settings_nav,
                self.element_color(UiElement::Settings),
            );
            let mark = layout.settings_nav.inset(12.0);
            rect(commands, mark, palette::ACCENT_GREEN);
        }

        let traffic_y = 20.0;
        rect(
            commands,
            Rect::new(20.0, traffic_y, 12.0, 12.0),
            palette::ACCENT_RED,
        );
        rect(
            commands,
            Rect::new(42.0, traffic_y, 12.0, 12.0),
            palette::ACCENT_YELLOW,
        );
        rect(
            commands,
            Rect::new(64.0, traffic_y, 12.0, 12.0),
            palette::ACCENT_GREEN,
        );
        rect(
            commands,
            Rect::new(layout.top_bar.width - 184.0, 18.0, 128.0, 20.0),
            palette::TOP_BAR_LINE,
        );

        if let Some(level) = self.status_level {
            let color = match level {
                StatusLevel::Info => palette::ACCENT_BLUE,
                StatusLevel::Warning => palette::ACCENT_YELLOW,
                StatusLevel::Error => palette::ACCENT_RED,
            };
            rect(commands, Rect::new(96.0, 18.0, 72.0, 20.0), color);
        }
    }

    fn draw_launcher(&self, commands: &mut Vec<DrawCommand>, layout: UiLayout) {
        rect(commands, layout.hero, palette::PANEL);
        rect(commands, layout.hero.inset(18.0), palette::PANEL_INSET);
        rect(
            commands,
            Rect::new(layout.hero.x + 28.0, layout.hero.y + 28.0, 180.0, 22.0),
            palette::ACCENT_BLUE,
        );
        rect(
            commands,
            Rect::new(
                layout.hero.x + 28.0,
                layout.hero.y + 66.0,
                (layout.hero.width * 0.52).max(80.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );
        rect(
            commands,
            Rect::new(
                layout.hero.x + 28.0,
                layout.hero.y + 92.0,
                (layout.hero.width * 0.38).max(64.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );

        rect(
            commands,
            layout.start_button,
            self.element_color(UiElement::Start),
        );
        rect(
            commands,
            Rect::new(
                layout.start_button.x + 18.0,
                layout.start_button.y + 18.0,
                layout.start_button.width * 0.42,
                20.0,
            ),
            palette::ON_ACTION,
        );
        rect(
            commands,
            layout.open_project_button,
            self.element_color(UiElement::OpenProject),
        );

        rect(
            commands,
            layout.settings_button,
            self.element_color(UiElement::Settings),
        );
        let center_x = layout.settings_button.x + layout.settings_button.width * 0.5 - 18.0;
        rect(
            commands,
            Rect::new(center_x, layout.settings_button.y + 28.0, 36.0, 36.0),
            palette::ACCENT_YELLOW,
        );
        rect(
            commands,
            Rect::new(
                layout.settings_button.x + 20.0,
                layout.settings_button.y + layout.settings_button.height - 34.0,
                (layout.settings_button.width - 40.0).max(0.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );

        let strip_y = layout.start_button.y + layout.start_button.height + 28.0;
        for index in 0..3 {
            rect(
                commands,
                Rect::new(
                    layout.content.x + (index as f32 * 126.0),
                    strip_y,
                    96.0,
                    18.0,
                ),
                palette::PANEL_INSET,
            );
        }
    }

    fn draw_settings(&self, commands: &mut Vec<DrawCommand>, layout: UiLayout) {
        rect(commands, layout.hero, palette::PANEL);
        rect(
            commands,
            Rect::new(layout.hero.x + 28.0, layout.hero.y + 30.0, 154.0, 24.0),
            palette::ACCENT_YELLOW,
        );
        rect(
            commands,
            layout.back_button,
            self.element_color(UiElement::Back),
        );
        rect(
            commands,
            Rect::new(
                layout.back_button.x + 18.0,
                layout.back_button.y + 14.0,
                42.0,
                14.0,
            ),
            palette::ON_ACTION,
        );

        let rows_top = layout.back_button.y + layout.back_button.height + 28.0;
        for index in 0..4 {
            let row_y = rows_top + index as f32 * 56.0;
            rect(
                commands,
                Rect::new(layout.content.x, row_y, layout.content.width, 40.0),
                palette::PANEL_INSET,
            );
            rect(
                commands,
                Rect::new(layout.content.x + 18.0, row_y + 13.0, 180.0, 14.0),
                palette::MUTED_LINE,
            );
            rect(
                commands,
                Rect::new(
                    layout.content.x + layout.content.width - 74.0,
                    row_y + 10.0,
                    48.0,
                    20.0,
                ),
                if index % 2 == 0 {
                    palette::ACCENT_GREEN
                } else {
                    palette::MUTED_LINE
                },
            );
        }
    }

    fn draw_running(&self, commands: &mut Vec<DrawCommand>, layout: UiLayout) {
        let stage_margin = if layout.content.width >= 640.0 {
            32.0
        } else {
            16.0
        };
        let stage = layout.content.inset(stage_margin);
        rect(commands, stage, palette::STAGE);

        let stage_inner = fit_rect(stage.inset(18.0), 16.0 / 9.0);
        rect(commands, stage_inner, palette::STAGE_INNER);

        rect(
            commands,
            Rect::new(
                stage_inner.x + 24.0,
                stage_inner.y + stage_inner.height - 82.0,
                (stage_inner.width - 48.0).max(0.0),
                58.0,
            ),
            palette::TEXT_BOX,
        );
        rect(
            commands,
            Rect::new(stage_inner.x + 48.0, stage_inner.y + 40.0, 120.0, 20.0),
            palette::ACCENT_GREEN,
        );
        rect(
            commands,
            Rect::new(
                stage_inner.x + 48.0,
                stage_inner.y + 76.0,
                (stage_inner.width * 0.42).max(80.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );

        let meter_y = layout.content.y + layout.content.height - 22.0;
        for index in 0..5 {
            rect(
                commands,
                Rect::new(
                    layout.content.x + index as f32 * 34.0,
                    meter_y,
                    20.0,
                    8.0 + index as f32 * 2.0,
                ),
                palette::PANEL_INSET,
            );
        }
    }

    fn element_color(&self, element: UiElement) -> Color {
        if self.pressed == Some(element) {
            return palette::PRESSED;
        }
        if self.hovered == Some(element) {
            return palette::HOVERED;
        }

        match element {
            UiElement::Start => palette::ACTION,
            UiElement::OpenProject => palette::ACTION_SECONDARY,
            UiElement::Settings | UiElement::Back => palette::CONTROL,
        }
    }
}

fn rect(commands: &mut Vec<DrawCommand>, rect: Rect, color: Color) {
    if rect.width > 0.0 && rect.height > 0.0 {
        commands.push(DrawCommand::Rect(RectCommand { rect, color }));
    }
}

fn fit_rect(bounds: Rect, aspect_ratio: f32) -> Rect {
    if bounds.width <= 0.0 || bounds.height <= 0.0 || aspect_ratio <= 0.0 {
        return Rect::default();
    }

    let bounds_ratio = bounds.width / bounds.height;
    if bounds_ratio > aspect_ratio {
        let width = bounds.height * aspect_ratio;
        Rect::new(
            bounds.x + (bounds.width - width) * 0.5,
            bounds.y,
            width,
            bounds.height,
        )
    } else {
        let height = bounds.width / aspect_ratio;
        Rect::new(
            bounds.x,
            bounds.y + (bounds.height - height) * 0.5,
            bounds.width,
            height,
        )
    }
}

mod palette {
    use super::Color;

    pub const BACKGROUND: Color = Color::rgb_u8(18, 20, 23);
    pub const RUNTIME_BACKGROUND: Color = Color::rgb_u8(10, 12, 14);
    pub const TOP_BAR: Color = Color::rgb_u8(32, 35, 40);
    pub const TOP_BAR_LINE: Color = Color::rgb_u8(67, 74, 84);
    pub const SIDE_RAIL: Color = Color::rgb_u8(25, 28, 32);
    pub const PANEL: Color = Color::rgb_u8(42, 48, 54);
    pub const PANEL_INSET: Color = Color::rgb_u8(57, 65, 73);
    pub const STAGE: Color = Color::rgb_u8(20, 23, 28);
    pub const STAGE_INNER: Color = Color::rgb_u8(12, 14, 18);
    pub const TEXT_BOX: Color = Color::new(0.07, 0.08, 0.1, 0.84);
    pub const MUTED_LINE: Color = Color::rgb_u8(105, 116, 128);
    pub const CONTROL: Color = Color::rgb_u8(77, 86, 96);
    pub const HOVERED: Color = Color::rgb_u8(103, 118, 132);
    pub const PRESSED: Color = Color::rgb_u8(70, 151, 137);
    pub const ACTION: Color = Color::rgb_u8(37, 130, 177);
    pub const ACTION_SECONDARY: Color = Color::rgb_u8(72, 91, 109);
    pub const ON_ACTION: Color = Color::rgb_u8(224, 230, 235);
    pub const ACCENT_BLUE: Color = Color::rgb_u8(77, 163, 214);
    pub const ACCENT_GREEN: Color = Color::rgb_u8(76, 175, 140);
    pub const ACCENT_YELLOW: Color = Color::rgb_u8(232, 181, 83);
    pub const ACCENT_RED: Color = Color::rgb_u8(214, 90, 82);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_inside_points_only() {
        let rect = Rect::new(10.0, 20.0, 100.0, 50.0);

        assert!(rect.contains(Point::new(10.0, 20.0)));
        assert!(rect.contains(Point::new(109.9, 69.9)));
        assert!(!rect.contains(Point::new(110.0, 70.0)));
        assert!(!rect.contains(Point::new(9.9, 20.0)));
    }

    #[test]
    fn launcher_layout_hits_interactive_regions() {
        let layout = UiLayout::new(Size::new(960.0, 600.0), Panel::Launcher);

        assert_eq!(
            layout.hit_test(Point::new(
                layout.start_button.x + 12.0,
                layout.start_button.y + 12.0
            )),
            Some(UiElement::Start)
        );
        assert_eq!(
            layout.hit_test(Point::new(
                layout.open_project_button.x + 4.0,
                layout.open_project_button.y + 4.0
            )),
            Some(UiElement::OpenProject)
        );
        assert_eq!(
            layout.hit_test(Point::new(
                layout.settings_button.x + 12.0,
                layout.settings_button.y + 12.0
            )),
            Some(UiElement::Settings)
        );
    }

    #[test]
    fn clicking_settings_switches_panel() {
        let mut engine = Engine::new(EngineConfig::default());
        let layout = engine.layout();
        let point = Point::new(
            layout.settings_button.x + 8.0,
            layout.settings_button.y + 8.0,
        );

        engine.handle_event(EngineEvent::CursorMoved { position: point });
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Pressed,
        });
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Released,
        });

        let view_model = engine.view_model();
        assert_eq!(view_model.panel, Panel::Settings);
        assert_eq!(view_model.last_action, Some(UiAction::SettingsOpened));
        assert_eq!(engine.take_last_action(), Some(UiAction::SettingsOpened));
        assert_eq!(engine.take_last_action(), None);
    }

    #[test]
    fn redraw_between_press_and_release_keeps_press_state() {
        let mut engine = Engine::new(EngineConfig::default());
        let layout = engine.layout();
        let point = Point::new(layout.start_button.x + 8.0, layout.start_button.y + 8.0);

        engine.handle_event(EngineEvent::CursorMoved { position: point });
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Pressed,
        });
        engine.set_panel(Panel::Launcher);
        let _frame = engine.tick(FrameInput::new(Size::new(960.0, 600.0), 0.016));
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Released,
        });

        assert_eq!(engine.take_last_action(), Some(UiAction::LaunchRequested));
    }

    #[test]
    fn draw_list_reflects_hover_state() {
        let mut engine = Engine::new(EngineConfig::default());
        let idle = engine.tick(FrameInput::new(Size::new(960.0, 600.0), 0.0));
        let layout = engine.layout();

        engine.handle_event(EngineEvent::CursorMoved {
            position: Point::new(layout.start_button.x + 10.0, layout.start_button.y + 10.0),
        });
        let hovered = engine.tick(FrameInput::new(Size::new(960.0, 600.0), 0.0));

        assert_eq!(idle.draw_commands.len(), hovered.draw_commands.len());
        assert_ne!(idle.draw_commands, hovered.draw_commands);
        assert_eq!(engine.view_model().hovered, Some(UiElement::Start));
    }

    #[test]
    fn running_frame_draws_stage_shell() {
        let mut engine = Engine::new(EngineConfig::default());
        let frame = engine.tick_running(FrameInput::new(Size::new(960.0, 600.0), 0.0));

        assert_eq!(frame.clear_color, palette::RUNTIME_BACKGROUND);
        assert!(frame.draw_commands.len() >= 10);
        assert_eq!(engine.panel(), Panel::Launcher);
    }

    #[test]
    fn fit_rect_preserves_aspect_ratio_inside_bounds() {
        let bounds = Rect::new(0.0, 0.0, 400.0, 400.0);
        let fitted = fit_rect(bounds, 16.0 / 9.0);

        assert!(bounds.contains(Point::new(fitted.x, fitted.y)));
        assert!(fitted.width <= bounds.width);
        assert!(fitted.height <= bounds.height);
        assert!((fitted.width / fitted.height - 16.0 / 9.0).abs() < 0.001);
    }
}

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    io::{self, Cursor, Read, Seek},
    sync::Arc,
};

pub use krkr_font::{FontSpec, TextStyle};

pub trait ResourceStream: Read + Seek + Send {}

impl<T> ResourceStream for T where T: Read + Seek + Send {}

pub trait ResourceDataSource: Send + Sync {
    fn byte_len(&self) -> u64;

    fn as_bytes(&self) -> io::Result<Cow<'_, [u8]>>;

    fn to_arc_bytes(&self) -> io::Result<Arc<[u8]>> {
        match self.as_bytes()? {
            Cow::Borrowed(bytes) => Ok(Arc::from(bytes)),
            Cow::Owned(bytes) => Ok(Arc::from(bytes)),
        }
    }

    fn open_stream(&self) -> io::Result<Box<dyn ResourceStream>>;
}

#[derive(Clone)]
pub struct ResourceData {
    source: Arc<dyn ResourceDataSource>,
}

impl ResourceData {
    pub fn new(source: Arc<dyn ResourceDataSource>) -> Self {
        Self { source }
    }

    pub fn from_bytes(bytes: Arc<[u8]>) -> Self {
        Self::new(Arc::new(SharedBytesResourceData { bytes }))
    }

    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self::from_bytes(Arc::from(bytes))
    }

    pub fn byte_len(&self) -> u64 {
        self.source.byte_len()
    }

    pub fn as_bytes(&self) -> io::Result<Cow<'_, [u8]>> {
        self.source.as_bytes()
    }

    pub fn to_arc_bytes(&self) -> io::Result<Arc<[u8]>> {
        self.source.to_arc_bytes()
    }

    pub fn open_stream(&self) -> io::Result<Box<dyn ResourceStream>> {
        self.source.open_stream()
    }
}

struct SharedBytesResourceData {
    bytes: Arc<[u8]>,
}

impl ResourceDataSource for SharedBytesResourceData {
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn as_bytes(&self) -> io::Result<Cow<'_, [u8]>> {
        Ok(Cow::Borrowed(self.bytes.as_ref()))
    }

    fn to_arc_bytes(&self) -> io::Result<Arc<[u8]>> {
        Ok(Arc::clone(&self.bytes))
    }

    fn open_stream(&self) -> io::Result<Box<dyn ResourceStream>> {
        Ok(Box::new(Cursor::new(Arc::clone(&self.bytes))))
    }
}

pub trait ResourceProvider: Send + Sync {
    fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>>;

    fn exists(&self, path: &str) -> bool;

    fn data(&self, path: &str) -> io::Result<ResourceData> {
        let mut stream = self.open(path)?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        Ok(ResourceData::from_vec(bytes))
    }

    fn byte_len(&self, path: &str) -> io::Result<Option<u64>> {
        let mut stream = self.open(path)?;
        let current = stream.stream_position().ok();
        let len = stream.seek(io::SeekFrom::End(0)).ok();
        if let Some(position) = current {
            let _ = stream.seek(io::SeekFrom::Start(position));
        }
        Ok(len)
    }

    fn revision(&self) -> u64 {
        0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AudioInstanceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioBus {
    Master,
    Bgm,
    SoundEffect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioLoadPolicy {
    Auto,
    Streaming,
    StaticCached,
    StaticUncached,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AudioSourceRef {
    pub storage: String,
}

impl AudioSourceRef {
    pub fn new(storage: impl Into<String>) -> Self {
        Self {
            storage: storage.into(),
        }
    }

    pub fn storage(&self) -> &str {
        &self.storage
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioCommand {
    Play {
        id: AudioInstanceId,
        bus: AudioBus,
        source: AudioSourceRef,
        load_policy: AudioLoadPolicy,
        looping: bool,
        volume: f32,
    },
    Preload {
        source: AudioSourceRef,
        load_policy: AudioLoadPolicy,
    },
    Stop {
        id: AudioInstanceId,
        fade_seconds: f32,
    },
    SetVolume {
        id: AudioInstanceId,
        volume: f32,
        fade_seconds: f32,
    },
    Pause {
        id: AudioInstanceId,
        fade_seconds: f32,
    },
    Resume {
        id: AudioInstanceId,
        fade_seconds: f32,
    },
    StopBus {
        bus: AudioBus,
        fade_seconds: f32,
    },
    SetBusVolume {
        bus: AudioBus,
        volume: f32,
        fade_seconds: f32,
    },
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

#[derive(Clone, Debug, PartialEq)]
pub struct TextCommand {
    pub position: Point,
    pub text: String,
    pub color: Color,
    pub size: f32,
    pub font: FontSpec,
    pub style: TextStyle,
}

pub type TextureId = u64;
pub type LayerId = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UploadedImageState {
    width: u32,
    height: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageUpload {
    pub texture_id: TextureId,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

impl ImageUpload {
    pub fn new(texture_id: TextureId, width: u32, height: u32, rgba: Arc<[u8]>) -> Self {
        Self {
            texture_id,
            width,
            height,
            rgba,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageCommand {
    pub texture_id: TextureId,
    pub rect: Rect,
    pub source_rect: Rect,
    pub texture_size: Size,
    pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawCommand {
    Rect(RectCommand),
    Text(TextCommand),
    Image(ImageCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TransitionMethod {
    Crossfade = 0,
    Universal = 1,
    Scroll = 2,
    Wave = 3,
    Mosaic = 4,
    Turn = 5,
    RotateZoom = 6,
    RotateVanish = 7,
    RotateSwap = 8,
    Ripple = 9,
}

impl TransitionMethod {
    pub fn from_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "universal" => Self::Universal,
            "scroll" => Self::Scroll,
            "wave" => Self::Wave,
            "mosaic" => Self::Mosaic,
            "turn" => Self::Turn,
            "rotatezoom" => Self::RotateZoom,
            "rotatevanish" => Self::RotateVanish,
            "rotateswap" => Self::RotateSwap,
            "ripple" => Self::Ripple,
            "crossfade" | "" => Self::Crossfade,
            _ => Self::Crossfade,
        }
    }

    pub const fn as_code(self) -> f32 {
        self as u8 as f32
    }

    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Crossfade => "crossfade",
            Self::Universal => "universal",
            Self::Scroll => "scroll",
            Self::Wave => "wave",
            Self::Mosaic => "mosaic",
            Self::Turn => "turn",
            Self::RotateZoom => "rotatezoom",
            Self::RotateVanish => "rotatevanish",
            Self::RotateSwap => "rotateswap",
            Self::Ripple => "ripple",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TransitionScrollFrom {
    Left = 0,
    Top = 1,
    Right = 2,
    Bottom = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TransitionScrollStay {
    NoStay = 0,
    StayDest = 1,
    StaySrc = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransitionParams {
    pub method: TransitionMethod,
    pub vague: f32,
    pub scroll_from: TransitionScrollFrom,
    pub scroll_stay: TransitionScrollStay,
    pub wave_type: f32,
    pub max_h: f32,
    pub max_omega: f32,
    pub bg_color1: Color,
    pub bg_color2: Color,
    pub max_size: f32,
    pub bg_color: Color,
    pub factor: f32,
    pub accel: f32,
    pub twist: f32,
    pub twist_accel: f32,
    pub center_x: f32,
    pub center_y: f32,
    pub ripple_width: f32,
    pub roundness: f32,
    pub speed: f32,
    pub max_drift: f32,
}

impl Default for TransitionParams {
    fn default() -> Self {
        Self {
            method: TransitionMethod::Crossfade,
            vague: 64.0,
            scroll_from: TransitionScrollFrom::Left,
            scroll_stay: TransitionScrollStay::NoStay,
            wave_type: 0.0,
            max_h: 50.0,
            max_omega: 0.2,
            bg_color1: Color::new(0.0, 0.0, 0.0, 1.0),
            bg_color2: Color::new(0.0, 0.0, 0.0, 1.0),
            max_size: 30.0,
            bg_color: Color::new(0.0, 0.0, 0.0, 1.0),
            factor: 1.0,
            accel: 0.0,
            twist: 2.0,
            twist_accel: -2.0,
            center_x: -1.0,
            center_y: -1.0,
            ripple_width: 128.0,
            roundness: 1.0,
            speed: 6.0,
            max_drift: 24.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameTransition {
    pub method: String,
    pub progress: f32,
    pub params: TransitionParams,
    pub rule_texture_id: Option<TextureId>,
    pub rule_image_upload: Option<ImageUpload>,
    pub frozen_draw_commands: Vec<DrawCommand>,
    pub frozen_image_uploads: Vec<ImageUpload>,
}

impl FrameTransition {
    pub fn crossfade(
        progress: f32,
        frozen_draw_commands: Vec<DrawCommand>,
        frozen_image_uploads: Vec<ImageUpload>,
    ) -> Self {
        Self {
            method: "crossfade".to_string(),
            progress: progress.clamp(0.0, 1.0),
            params: TransitionParams::default(),
            rule_texture_id: None,
            rule_image_upload: None,
            frozen_draw_commands,
            frozen_image_uploads,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameOutput {
    pub clear_color: Color,
    pub clip: Option<Rect>,
    pub draw_commands: Vec<DrawCommand>,
    pub image_uploads: Vec<ImageUpload>,
    pub transition: Option<FrameTransition>,
}

impl FrameOutput {
    pub fn new(clear_color: Color, draw_commands: Vec<DrawCommand>) -> Self {
        Self {
            clear_color,
            clip: None,
            draw_commands,
            image_uploads: Vec::new(),
            transition: None,
        }
    }

    pub fn with_clip(mut self, clip: Rect) -> Self {
        self.clip = Some(clip);
        self
    }

    pub fn with_image_uploads(mut self, image_uploads: Vec<ImageUpload>) -> Self {
        self.image_uploads = image_uploads;
        self
    }

    pub fn with_transition(mut self, transition: Option<FrameTransition>) -> Self {
        self.transition = transition;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerImage {
    pub upload: ImageUpload,
}

impl LayerImage {
    pub fn new(texture_id: TextureId, width: u32, height: u32, rgba: Arc<[u8]>) -> Self {
        Self {
            upload: ImageUpload::new(texture_id, width, height, rgba),
        }
    }

    pub fn size(&self) -> Size {
        Size::new(self.upload.width as f32, self.upload.height as f32)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerNode {
    pub id: LayerId,
    pub name: String,
    pub parent: Option<LayerId>,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub image_left: f32,
    pub image_top: f32,
    pub image_width: f32,
    pub image_height: f32,
    pub visible: bool,
    pub renderable: bool,
    pub enabled: bool,
    pub node_enabled: bool,
    pub opacity: u8,
    pub z_order: i32,
    pub layer_type: i32,
    pub face: i32,
    pub hit_type: i32,
    pub hit_threshold: i32,
    pub image: Option<LayerImage>,
}

impl LayerNode {
    pub fn new(
        id: LayerId,
        name: impl Into<String>,
        parent: Option<LayerId>,
        z_order: i32,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            parent,
            left: 0.0,
            top: 0.0,
            width: 0.0,
            height: 0.0,
            image_left: 0.0,
            image_top: 0.0,
            image_width: 0.0,
            image_height: 0.0,
            visible: false,
            renderable: true,
            enabled: true,
            node_enabled: true,
            opacity: 255,
            z_order,
            layer_type: 2,
            face: 128,
            hit_type: 0,
            hit_threshold: 0,
            image: None,
        }
    }

    pub fn rect(&self) -> Rect {
        Rect::new(self.left, self.top, self.width, self.height)
    }

    pub fn image_rect(&self) -> Rect {
        Rect::new(
            self.image_left,
            self.image_top,
            self.image_width,
            self.image_height,
        )
    }

    fn hit_test(&self, origin: Point, point: Point) -> bool {
        match self.hit_type {
            1 => false,
            _ => self.alpha_hit_test(origin, point),
        }
    }

    fn alpha_hit_test(&self, origin: Point, point: Point) -> bool {
        let Some(image) = &self.image else {
            return self.hit_threshold <= 0;
        };
        let x = (point.x - origin.x - self.image_left).floor() as i64;
        let y = (point.y - origin.y - self.image_top).floor() as i64;
        if x < 0 || y < 0 || x >= image.upload.width as i64 || y >= image.upload.height as i64 {
            return false;
        }
        let index = ((y as u32 * image.upload.width + x as u32) * 4 + 3) as usize;
        i32::from(image.upload.rgba[index]) >= self.hit_threshold
    }

    fn image_command(
        &self,
        origin: Point,
        clip: Rect,
        inherited_opacity: f32,
    ) -> Option<ImageCommand> {
        let image = self.image.as_ref()?;
        if self.width <= 0.0 || self.height <= 0.0 {
            return None;
        }

        let texture_size = image.size();
        let image_width = texture_size.width;
        let image_height = texture_size.height;
        let layer_x0 = origin.x;
        let layer_y0 = origin.y;
        let layer_x1 = origin.x + self.width;
        let layer_y1 = origin.y + self.height;
        let image_x0 = origin.x + self.image_left;
        let image_y0 = origin.y + self.image_top;
        let image_x1 = image_x0 + image_width;
        let image_y1 = image_y0 + image_height;

        let target_x0 = layer_x0.max(image_x0).max(clip.x);
        let target_y0 = layer_y0.max(image_y0).max(clip.y);
        let target_x1 = layer_x1.min(image_x1).min(clip.x + clip.width);
        let target_y1 = layer_y1.min(image_y1).min(clip.y + clip.height);
        if target_x1 <= target_x0 || target_y1 <= target_y0 {
            return None;
        }

        Some(ImageCommand {
            texture_id: image.upload.texture_id,
            rect: Rect::new(
                target_x0,
                target_y0,
                target_x1 - target_x0,
                target_y1 - target_y0,
            ),
            source_rect: Rect::new(
                target_x0 - image_x0,
                target_y0 - image_y0,
                target_x1 - target_x0,
                target_y1 - target_y0,
            ),
            texture_size,
            opacity: inherited_opacity * self.opacity as f32 / 255.0,
        })
    }

    pub fn set_image(&mut self, image: LayerImage) {
        let size = image.size();
        self.image_width = size.width;
        self.image_height = size.height;
        if self.width <= 0.0 || self.height <= 0.0 {
            self.width = size.width;
            self.height = size.height;
        }
        self.image = Some(image);
    }

    pub fn clear_image(&mut self) {
        self.image = None;
        self.image_width = 0.0;
        self.image_height = 0.0;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerTree {
    layers: BTreeMap<LayerId, LayerNode>,
    next_layer_id: LayerId,
}

impl Default for LayerTree {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerTree {
    pub fn new() -> Self {
        Self {
            layers: BTreeMap::new(),
            next_layer_id: 1,
        }
    }

    pub fn create_layer(
        &mut self,
        name: impl Into<String>,
        parent: Option<LayerId>,
        z_order: i32,
    ) -> LayerId {
        let id = self.next_layer_id;
        self.next_layer_id = self.next_layer_id.saturating_add(1);
        self.layers
            .insert(id, LayerNode::new(id, name, parent, z_order));
        id
    }

    pub fn layer(&self, id: LayerId) -> Option<&LayerNode> {
        self.layers.get(&id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut LayerNode> {
        self.layers.get_mut(&id)
    }

    pub fn layers(&self) -> impl Iterator<Item = &LayerNode> {
        self.layers.values()
    }

    pub fn remove_layer(&mut self, id: LayerId) -> Option<LayerNode> {
        self.layers.remove(&id)
    }

    pub fn set_parent(&mut self, id: LayerId, parent: Option<LayerId>) -> bool {
        if parent == Some(id) || parent.is_some_and(|parent| self.is_descendant(parent, id)) {
            return false;
        }
        let Some(layer) = self.layers.get_mut(&id) else {
            return false;
        };
        layer.parent = parent;
        true
    }

    pub fn absolute_position(&self, id: LayerId) -> Option<Point> {
        let mut x = 0.0;
        let mut y = 0.0;
        let mut current = Some(id);
        while let Some(layer_id) = current {
            let layer = self.layers.get(&layer_id)?;
            x += layer.left;
            y += layer.top;
            current = layer.parent;
        }
        Some(Point::new(x, y))
    }

    pub fn hit_test(&self, point: Point) -> Option<LayerId> {
        self.hit_test_all(point).into_iter().next()
    }

    pub fn hit_test_all(&self, point: Point) -> Vec<LayerId> {
        let mut hits = Vec::new();
        let roots = self.sorted_children(None);
        for root in roots.into_iter().rev() {
            self.hit_test_layer_all(root.id, Point::new(0.0, 0.0), point, &mut hits);
        }
        hits
    }

    pub fn draw_model(&self) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        self.draw_model_suppressing_images(&BTreeSet::new())
    }

    pub fn draw_model_suppressing_images(
        &self,
        suppressed_images: &BTreeSet<LayerId>,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        let mut model = LayerDrawModel {
            suppressed_images,
            commands: Vec::new(),
            uploads: Vec::new(),
        };
        let clip = Rect::new(0.0, 0.0, f32::MAX / 4.0, f32::MAX / 4.0);
        for root in self.sorted_children(None) {
            self.draw_layer(root.id, Point::new(0.0, 0.0), clip, 1.0, &mut model);
        }
        (model.commands, model.uploads)
    }

    fn draw_layer(
        &self,
        id: LayerId,
        parent_origin: Point,
        parent_clip: Rect,
        parent_opacity: f32,
        model: &mut LayerDrawModel<'_>,
    ) {
        let Some(layer) = self.layers.get(&id) else {
            return;
        };
        if !layer.renderable || !layer.visible || layer.opacity == 0 {
            return;
        }
        let origin = Point::new(parent_origin.x + layer.left, parent_origin.y + layer.top);
        let layer_rect = Rect::new(origin.x, origin.y, layer.width, layer.height);
        let Some(clip) = intersect_rect(parent_clip, layer_rect) else {
            return;
        };
        let opacity = parent_opacity * layer.opacity as f32 / 255.0;

        if !model.suppressed_images.contains(&id)
            && let Some(command) = layer.image_command(origin, clip, parent_opacity)
        {
            model.commands.push(DrawCommand::Image(command));
            if let Some(image) = &layer.image {
                model.uploads.push(image.upload.clone());
            }
        }

        for child in self.sorted_children(Some(id)) {
            self.draw_layer(child.id, origin, clip, opacity, model);
        }
    }

    fn hit_test_layer_all(
        &self,
        id: LayerId,
        parent_origin: Point,
        point: Point,
        hits: &mut Vec<LayerId>,
    ) {
        let Some(layer) = self.layers.get(&id) else {
            return;
        };
        if !layer.renderable
            || !layer.visible
            || !layer.enabled
            || !layer.node_enabled
            || layer.opacity == 0
            || layer.width <= 0.0
            || layer.height <= 0.0
        {
            return;
        }

        let origin = Point::new(parent_origin.x + layer.left, parent_origin.y + layer.top);
        let rect = Rect::new(origin.x, origin.y, layer.width, layer.height);
        if !rect.contains(point) {
            return;
        }

        for child in self.sorted_children(Some(id)).into_iter().rev() {
            self.hit_test_layer_all(child.id, origin, point, hits);
        }

        if layer.hit_test(origin, point) {
            hits.push(id);
        }
    }

    fn sorted_children(&self, parent: Option<LayerId>) -> Vec<&LayerNode> {
        let mut children = self
            .layers
            .values()
            .filter(|layer| layer.parent == parent)
            .collect::<Vec<_>>();
        children.sort_by_key(|layer| (layer.z_order, layer.id));
        children
    }

    fn is_descendant(&self, id: LayerId, ancestor: LayerId) -> bool {
        let mut current = Some(id);
        while let Some(layer_id) = current {
            if layer_id == ancestor {
                return true;
            }
            current = self.layers.get(&layer_id).and_then(|layer| layer.parent);
        }
        false
    }
}

struct LayerDrawModel<'a> {
    suppressed_images: &'a BTreeSet<LayerId>,
    commands: Vec<DrawCommand>,
    uploads: Vec<ImageUpload>,
}

fn collect_image_texture_ids(commands: &[DrawCommand], texture_ids: &mut BTreeSet<TextureId>) {
    for command in commands {
        if let DrawCommand::Image(image) = command {
            texture_ids.insert(image.texture_id);
        }
    }
}

fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageLayerModel {
    pub lines: Vec<String>,
    pub waiting_for_click: bool,
    pub page: usize,
    pub font: FontSpec,
    pub style: TextStyle,
    pub cursor_x: i32,
    pub cursor_y: i32,
}

impl MessageLayerModel {
    pub fn clear(&mut self) {
        self.lines.clear();
        self.waiting_for_click = false;
        self.page = 0;
        self.font = FontSpec::default();
        self.style = TextStyle::default();
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    pub fn clear_text(&mut self) {
        self.lines.clear();
    }

    pub fn append_text(&mut self, text: &str) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        if let Some(line) = self.lines.last_mut() {
            line.push_str(text);
        }
    }

    pub fn newline(&mut self) {
        self.lines.push(String::new());
    }

    pub fn page_break(&mut self) {
        self.page = self.page.saturating_add(1);
        self.waiting_for_click = true;
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
    Tab,
    Left,
    Up,
    Right,
    Down,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Shift,
    Control,
    Alt,
    Character(char),
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
    MouseWheel {
        delta: i32,
    },
    KeyboardInput {
        key: EngineKey,
        state: ButtonState,
        repeat: bool,
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
    uploaded_images: BTreeMap<TextureId, UploadedImageState>,
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
            uploaded_images: BTreeMap::new(),
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
                ..
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
                ..
            } => {
                self.activate(self.hovered);
            }
            EngineEvent::PointerInput { .. }
            | EngineEvent::MouseWheel { .. }
            | EngineEvent::KeyboardInput { .. } => {}
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
        self.tick_running_with_message(input, &MessageLayerModel::default())
    }

    pub fn tick_running_with_message(
        &mut self,
        input: FrameInput,
        message: &MessageLayerModel,
    ) -> FrameOutput {
        if !input.viewport_size.is_empty() {
            self.viewport_size = input.viewport_size;
        }

        let layout = UiLayout::new(self.viewport_size, Panel::Launcher);
        let mut draw_commands = Vec::with_capacity(24);
        self.draw_shell(&mut draw_commands, layout);
        self.draw_running(&mut draw_commands, layout, message);

        FrameOutput::new(palette::RUNTIME_BACKGROUND, draw_commands)
    }

    pub fn tick_running_with_layers(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
    ) -> FrameOutput {
        self.tick_running_with_layers_suppressing_images(input, layers, message, &BTreeSet::new())
    }

    pub fn tick_running_with_layers_suppressing_images(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
        suppressed_images: &BTreeSet<LayerId>,
    ) -> FrameOutput {
        let output =
            self.running_layer_frame_output(input, layers, message, suppressed_images, None);
        self.finalize_frame_output(output)
    }

    pub fn tick_running_with_layers_suppressing_images_and_transition(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
        suppressed_images: &BTreeSet<LayerId>,
        transition: Option<FrameTransition>,
    ) -> FrameOutput {
        let output =
            self.running_layer_frame_output(input, layers, message, suppressed_images, transition);
        self.finalize_frame_output(output)
    }

    fn running_layer_frame_output(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
        suppressed_images: &BTreeSet<LayerId>,
        transition: Option<FrameTransition>,
    ) -> FrameOutput {
        if !input.viewport_size.is_empty() {
            self.viewport_size = input.viewport_size;
        }

        let (mut draw_commands, image_uploads) =
            layers.draw_model_suppressing_images(suppressed_images);
        self.draw_message_overlay(&mut draw_commands, message);

        FrameOutput::new(palette::RUNTIME_BACKGROUND, draw_commands)
            .with_image_uploads(image_uploads)
            .with_transition(transition)
    }

    fn filter_new_image_uploads(&mut self, uploads: Vec<ImageUpload>) -> Vec<ImageUpload> {
        uploads
            .into_iter()
            .filter(|upload| {
                let state = UploadedImageState {
                    width: upload.width,
                    height: upload.height,
                };
                if self.uploaded_images.get(&upload.texture_id) == Some(&state) {
                    return false;
                }
                self.uploaded_images.insert(upload.texture_id, state);
                true
            })
            .collect()
    }

    fn finalize_frame_output(&mut self, mut output: FrameOutput) -> FrameOutput {
        output.image_uploads = self.filter_new_image_uploads(output.image_uploads);
        if let Some(transition) = &mut output.transition {
            transition.frozen_image_uploads =
                self.filter_new_image_uploads(std::mem::take(&mut transition.frozen_image_uploads));
            if let Some(upload) = transition.rule_image_upload.take() {
                let mut uploads = self.filter_new_image_uploads(vec![upload]);
                transition.rule_image_upload = uploads.pop();
            }
        }
        let mut referenced_textures = BTreeSet::new();
        collect_image_texture_ids(&output.draw_commands, &mut referenced_textures);
        if let Some(transition) = &output.transition {
            collect_image_texture_ids(&transition.frozen_draw_commands, &mut referenced_textures);
            if let Some(texture_id) = transition.rule_texture_id {
                referenced_textures.insert(texture_id);
            }
        }
        self.uploaded_images
            .retain(|texture_id, _| referenced_textures.contains(texture_id));
        output
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

    fn draw_running(
        &self,
        commands: &mut Vec<DrawCommand>,
        layout: UiLayout,
        message: &MessageLayerModel,
    ) {
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
                70.0,
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

        let text_x = stage_inner.x + 42.0;
        let mut text_y = stage_inner.y + stage_inner.height - 66.0;
        let first_line = message.lines.len().saturating_sub(3);
        for line in message.lines.iter().skip(first_line) {
            text(
                commands,
                Point::new(text_x, text_y),
                line,
                palette::TEXT,
                18.0,
                Some((&message.font, &message.style)),
            );
            text_y += 21.0;
        }
        if message.waiting_for_click {
            rect(
                commands,
                Rect::new(
                    stage_inner.x + stage_inner.width - 58.0,
                    stage_inner.y + stage_inner.height - 36.0,
                    12.0,
                    12.0,
                ),
                palette::ACCENT_YELLOW,
            );
        }

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

    fn draw_message_overlay(&self, commands: &mut Vec<DrawCommand>, message: &MessageLayerModel) {
        if message.lines.is_empty() && !message.waiting_for_click {
            return;
        }

        let width = self.viewport_size.width.max(320.0);
        let height = self.viewport_size.height.max(240.0);
        let margin = if width >= 640.0 { 42.0 } else { 20.0 };
        let box_height = 116.0_f32.min((height - margin * 2.0).max(64.0));
        let box_rect = Rect::new(
            margin,
            (height - box_height - margin).max(margin),
            (width - margin * 2.0).max(0.0),
            box_height,
        );
        rect(commands, box_rect, palette::TEXT_BOX);

        let text_x = box_rect.x + 24.0;
        let mut text_y = box_rect.y + 20.0;
        let first_line = message.lines.len().saturating_sub(3);
        for line in message.lines.iter().skip(first_line) {
            text(
                commands,
                Point::new(text_x, text_y),
                line,
                palette::TEXT,
                18.0,
                Some((&message.font, &message.style)),
            );
            text_y += 23.0;
        }
        if message.waiting_for_click {
            rect(
                commands,
                Rect::new(
                    box_rect.x + box_rect.width - 30.0,
                    box_rect.y + box_rect.height - 30.0,
                    12.0,
                    12.0,
                ),
                palette::ACCENT_YELLOW,
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

fn text(
    commands: &mut Vec<DrawCommand>,
    position: Point,
    text: &str,
    color: Color,
    size: f32,
    font_style: Option<(&FontSpec, &TextStyle)>,
) {
    if !text.is_empty() && size > 0.0 {
        let (font, style) = match font_style {
            Some((font, style)) => (font.clone(), *style),
            None => {
                let font = FontSpec {
                    height: size,
                    ..FontSpec::default()
                };
                let style = TextStyle {
                    color: color_to_u8(color),
                    ..TextStyle::default()
                };
                (font, style)
            }
        };
        commands.push(DrawCommand::Text(TextCommand {
            position,
            text: text.to_string(),
            color,
            size,
            font,
            style,
        }));
    }
}

fn color_to_u8(color: Color) -> [u8; 4] {
    [
        (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.a.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
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
    pub const TEXT: Color = Color::rgb_u8(232, 238, 242);
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
    fn running_frame_draws_message_text() {
        let mut engine = Engine::new(EngineConfig::default());
        let mut message = MessageLayerModel::default();
        message.append_text("Hello");
        message.newline();
        message.append_text("World");

        let frame = engine
            .tick_running_with_message(FrameInput::new(Size::new(960.0, 600.0), 0.0), &message);

        assert!(
            frame
                .draw_commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Text(text) if text.text == "Hello"))
        );
        assert!(
            frame
                .draw_commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Text(text) if text.text == "World"))
        );
    }

    #[test]
    fn running_layer_uploads_are_emitted_only_once_per_texture() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let image = LayerImage::new(42, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("image", None, 0);
        {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let mut engine = Engine::new(EngineConfig::default());
        let first = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );
        let second = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );

        assert_eq!(first.image_uploads.len(), 1);
        assert!(second.image_uploads.is_empty());
        assert_eq!(first.draw_commands, second.draw_commands);
    }

    #[test]
    fn running_layer_uploads_are_reemitted_after_texture_leaves_frame() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let image = LayerImage::new(42, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("image", None, 0);
        {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let mut engine = Engine::new(EngineConfig::default());
        let first = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );
        layers.layer_mut(id).expect("layer").visible = false;
        let hidden = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );
        layers.layer_mut(id).expect("layer").visible = true;
        let visible_again = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );

        assert_eq!(first.image_uploads.len(), 1);
        assert!(hidden.image_uploads.is_empty());
        assert_eq!(visible_again.image_uploads.len(), 1);
        assert_eq!(visible_again.image_uploads[0].texture_id, 42);
    }

    #[test]
    fn layer_tree_draw_model_sorts_visible_images_by_z_order() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let low_image = LayerImage::new(1, 1, 1, pixels.clone());
        let high_image = LayerImage::new(2, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let high = layers.create_layer("high", None, 20);
        let low = layers.create_layer("low", None, 10);

        {
            let layer = layers.layer_mut(high).expect("high layer");
            layer.left = 20.0;
            layer.top = 30.0;
            layer.visible = true;
            layer.set_image(high_image);
        }
        {
            let layer = layers.layer_mut(low).expect("low layer");
            layer.left = 2.0;
            layer.top = 3.0;
            layer.visible = true;
            layer.set_image(low_image);
        }

        let (commands, uploads) = layers.draw_model();

        assert_eq!(uploads.len(), 2);
        assert!(matches!(
            &commands[..],
            [
                DrawCommand::Image(first),
                DrawCommand::Image(second)
            ] if first.texture_id == 1 && second.texture_id == 2
        ));
    }

    #[test]
    fn layer_tree_suppresses_layer_image_without_hiding_children() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let parent_image = LayerImage::new(1, 1, 1, pixels.clone());
        let child_image = LayerImage::new(2, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let parent = layers.create_layer("parent", None, 0);
        let child = layers.create_layer("child", Some(parent), 0);
        for (id, image) in [(parent, parent_image), (child, child_image)] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let (commands, uploads) = layers.draw_model_suppressing_images(&BTreeSet::from([parent]));

        assert_eq!(uploads.len(), 1);
        assert!(matches!(
            &commands[..],
            [DrawCommand::Image(image)] if image.texture_id == 2
        ));
    }

    #[test]
    fn layer_tree_draw_model_clips_negative_image_offsets_for_sprite_sheets() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255; 12 * 4]);
        let image = LayerImage::new(7, 12, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("button", None, 10);
        {
            let layer = layers.layer_mut(id).expect("button layer");
            layer.left = 100.0;
            layer.top = 20.0;
            layer.width = 4.0;
            layer.height = 1.0;
            layer.image_left = -4.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let (commands, uploads) = layers.draw_model();

        assert_eq!(uploads.len(), 1);
        assert!(matches!(
            &commands[..],
            [DrawCommand::Image(image)]
                if image.rect == Rect::new(100.0, 20.0, 4.0, 1.0)
                    && image.source_rect == Rect::new(4.0, 0.0, 4.0, 1.0)
                    && image.texture_size == Size::new(12.0, 1.0)
        ));
    }

    #[test]
    fn layer_tree_draw_model_clips_positive_image_offsets_to_layer_viewport() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255; 8 * 4]);
        let image = LayerImage::new(9, 8, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("inset", None, 10);
        {
            let layer = layers.layer_mut(id).expect("inset layer");
            layer.left = 10.0;
            layer.top = 5.0;
            layer.width = 4.0;
            layer.height = 1.0;
            layer.image_left = 2.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let (commands, _uploads) = layers.draw_model();

        assert!(matches!(
            &commands[..],
            [DrawCommand::Image(image)]
                if image.rect == Rect::new(12.0, 5.0, 2.0, 1.0)
                    && image.source_rect == Rect::new(0.0, 0.0, 2.0, 1.0)
        ));
    }

    #[test]
    fn layer_tree_hit_test_returns_topmost_visible_layer() {
        let mut layers = LayerTree::new();
        let low = layers.create_layer("low", None, 10);
        let high = layers.create_layer("high", None, 20);
        for id in [low, high] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.left = 10.0;
            layer.top = 20.0;
            layer.width = 30.0;
            layer.height = 40.0;
            layer.visible = true;
        }

        assert_eq!(layers.hit_test(Point::new(15.0, 25.0)), Some(high));
        assert_eq!(layers.hit_test(Point::new(50.0, 25.0)), None);

        layers.layer_mut(high).expect("high").visible = false;
        assert_eq!(layers.hit_test(Point::new(15.0, 25.0)), Some(low));
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

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::error::{Result, Span, TjsError};
use crate::frontend::{hir, syntax};

pub const MIR_VERSION: MirVersion = MirVersion { major: 0, minor: 2 };

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceFileId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SpanId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StringId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConstId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ObjectId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BlockId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExceptionRegionId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LocalId(pub u32);
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TempId(pub u32);

#[derive(Clone, Debug, PartialEq)]
pub struct MirModule {
    pub version: MirVersion,
    pub sources: Vec<SourceFile>,
    pub spans: Vec<SourceSpan>,
    pub strings: Vec<String>,
    pub constants: Vec<MirConst>,
    pub objects: Vec<MirObject>,
    pub top_level: ObjectId,
}

impl MirModule {
    pub fn validate(&self) -> Result<()> {
        if self.version != MIR_VERSION {
            return Err(TjsError::mir("unsupported MIR version"));
        }

        for source in &self.sources {
            let Some(actual) = self.sources.get(source.id.0 as usize) else {
                return Err(TjsError::mir("source id is outside source table"));
            };
            if actual.id != source.id {
                return Err(TjsError::mir("source id does not match source table index"));
            }
        }

        for span in &self.spans {
            if self
                .sources
                .get(span.file.0 as usize)
                .is_none_or(|source| source.id != span.file)
            {
                return Err(TjsError::mir("source span references an invalid file"));
            }
            if span.byte_start > span.byte_end || span.utf16_start > span.utf16_end {
                return Err(TjsError::mir("source span has inverted bounds"));
            }
        }

        let mut object_ids = BTreeSet::new();
        for object in &self.objects {
            if !object_ids.insert(object.id) {
                return Err(TjsError::mir("duplicate object id"));
            }
        }

        for constant in &self.constants {
            self.validate_const(constant)?;
        }

        for object in &self.objects {
            self.validate_object_refs(object)?;
            object.validate(self)?;
        }

        let top = self
            .objects
            .iter()
            .find(|object| object.id == self.top_level)
            .ok_or_else(|| TjsError::mir("top-level object is missing"))?;
        if top.context != ContextType::TopLevel {
            return Err(TjsError::mir("top-level object has non-top-level context"));
        }
        Ok(())
    }

    fn validate_const(&self, constant: &MirConst) -> Result<()> {
        match constant {
            MirConst::String(id) => self.require_string(*id),
            MirConst::CodeObject(id) => self.require_object(*id),
            MirConst::Void
            | MirConst::NullObject
            | MirConst::Integer(_)
            | MirConst::Real(_)
            | MirConst::Octet(_) => Ok(()),
        }
    }

    fn validate_object_refs(&self, object: &MirObject) -> Result<()> {
        self.require_string(object.name)?;
        if let Some(span) = object.source_span {
            self.require_span(span)?;
        }
        for target in [
            object.parent,
            object.prop_getter,
            object.prop_setter,
            object.super_class_getter,
        ]
        .into_iter()
        .flatten()
        {
            self.require_object(target)?;
        }
        for property in &object.properties {
            self.require_string(property.name)?;
            self.require_object(property.object)?;
        }
        Ok(())
    }

    fn require_string(&self, id: StringId) -> Result<()> {
        self.strings
            .get(id.0 as usize)
            .map(|_| ())
            .ok_or_else(|| TjsError::mir(format!("invalid string id {}", id.0)))
    }

    fn require_const(&self, id: ConstId) -> Result<()> {
        self.constants
            .get(id.0 as usize)
            .map(|_| ())
            .ok_or_else(|| TjsError::mir(format!("invalid constant id {}", id.0)))
    }

    fn require_object(&self, id: ObjectId) -> Result<()> {
        self.objects
            .iter()
            .find(|object| object.id == id)
            .map(|_| ())
            .ok_or_else(|| TjsError::mir(format!("invalid object id {}", id.0)))
    }

    fn require_span(&self, id: SpanId) -> Result<()> {
        self.spans
            .get(id.0 as usize)
            .map(|_| ())
            .ok_or_else(|| TjsError::mir(format!("invalid span id {}", id.0)))
    }

    pub fn snapshot(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!(
            "mir {}.{}\n",
            self.version.major, self.version.minor
        ));
        output.push_str("strings:\n");
        for (index, string) in self.strings.iter().enumerate() {
            output.push_str(&format!("  @{index} {string:?}\n"));
        }
        output.push_str("constants:\n");
        for (index, constant) in self.constants.iter().enumerate() {
            output.push_str(&format!("  #{index} {constant:?}\n"));
        }
        output.push_str("objects:\n");
        for object in &self.objects {
            output.push_str(&format!(
                "  object #{} {:?} name=@{} parent={:?}\n",
                object.id.0, object.context, object.name.0, object.parent
            ));
            if !object.args.declared.is_empty()
                || object.args.collapse_base.is_some()
                || object.args.unnamed_arg_array_base.is_some()
            {
                output.push_str(&format!("    args {:?}\n", object.args));
            }
            if !object.properties.is_empty() {
                output.push_str(&format!("    properties {:?}\n", object.properties));
            }
            if !object.exception_regions.is_empty() {
                output.push_str(&format!(
                    "    exception_regions {:?}\n",
                    object.exception_regions
                ));
            }
            for block in &object.blocks {
                output.push_str(&format!("    block #{}\n", block.id.0));
                for inst in &block.insts {
                    output.push_str(&format!("      {inst:?}\n"));
                }
                output.push_str(&format!("      -> {:?}\n", block.terminator));
            }
        }
        output
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceFile {
    pub id: SourceFileId,
    pub name: String,
    pub text_hash: Option<[u8; 32]>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceSpan {
    pub file: SourceFileId,
    pub byte_start: u32,
    pub byte_end: u32,
    pub utf16_start: u32,
    pub utf16_end: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MirObject {
    pub id: ObjectId,
    pub name: StringId,
    pub context: ContextType,
    pub parent: Option<ObjectId>,
    pub prop_getter: Option<ObjectId>,
    pub prop_setter: Option<ObjectId>,
    pub super_class_getter: Option<ObjectId>,
    pub args: FunctionArgs,
    pub frame: FrameDecl,
    pub properties: Vec<PropertyRegistration>,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    pub exception_regions: Vec<ExceptionRegion>,
    pub source_span: Option<SpanId>,
}

impl MirObject {
    fn validate(&self, module: &MirModule) -> Result<()> {
        let block_ids: BTreeSet<_> = self.blocks.iter().map(|block| block.id).collect();
        if !block_ids.contains(&self.entry) {
            return Err(TjsError::mir(format!(
                "object {} entry block is missing",
                self.id.0
            )));
        }

        let region_ids: BTreeSet<_> = self
            .exception_regions
            .iter()
            .map(|region| region.id)
            .collect();
        for region in &self.exception_regions {
            if let Some(parent) = region.parent
                && !region_ids.contains(&parent)
            {
                return Err(TjsError::mir("exception region has invalid parent"));
            }
            require_block(&block_ids, region.entry)?;
            require_block(&block_ids, region.catch)?;
            for block in &region.protected_blocks {
                require_block(&block_ids, *block)?;
            }
            self.validate_slot(region.exception_slot)?;
        }

        for block in &self.blocks {
            if !block.params.is_empty() {
                return Err(TjsError::mir("MIR v0 blocks must not have params"));
            }
            if let Some(span) = block.source_span {
                module.require_span(span)?;
            }
            for inst in &block.insts {
                self.validate_inst(module, inst)?;
            }
            self.validate_terminator(module, &region_ids, &block_ids, &block.terminator)?;
        }

        if self.context == ContextType::TopLevel && self.parent.is_some() {
            return Err(TjsError::mir("top-level object must not have a parent"));
        }
        if self.context == ContextType::Property
            && self.prop_getter.is_none()
            && self.prop_setter.is_none()
        {
            return Err(TjsError::mir("property object has no getter or setter"));
        }
        Ok(())
    }

    fn validate_inst(&self, module: &MirModule, inst: &MirInst) -> Result<()> {
        match inst {
            MirInst::Nop | MirInst::RegisterMembers | MirInst::Debugger => Ok(()),
            MirInst::LoadConst { dst, value } => {
                self.validate_slot(*dst)?;
                module.require_const(*value)
            }
            MirInst::Copy { dst, src } => {
                self.validate_slot(*dst)?;
                self.validate_value(module, *src)
            }
            MirInst::Clear { dst } => self.validate_slot(*dst),
            MirInst::ReadPlace { dst, place } => {
                self.validate_slot(*dst)?;
                self.validate_place(module, place)
            }
            MirInst::Assign {
                place,
                value,
                result,
            } => {
                self.validate_place(module, place)?;
                self.validate_value(module, *value)?;
                if let Some(result) = result {
                    self.validate_slot(*result)?;
                }
                Ok(())
            }
            MirInst::AssignOp {
                place, rhs, result, ..
            } => {
                self.validate_place(module, place)?;
                self.validate_value(module, *rhs)?;
                if let Some(result) = result {
                    self.validate_slot(*result)?;
                }
                Ok(())
            }
            MirInst::Swap { left, right } => {
                self.validate_place(module, left)?;
                self.validate_place(module, right)
            }
            MirInst::Update { place, result, .. } => {
                self.validate_place(module, place)?;
                if let Some(result) = result {
                    self.validate_slot(*result)?;
                }
                Ok(())
            }
            MirInst::Unary { dst, src, .. }
            | MirInst::Convert { dst, src, .. }
            | MirInst::ToBoolean { dst, src } => {
                self.validate_slot(*dst)?;
                self.validate_value(module, *src)
            }
            MirInst::Binary { dst, lhs, rhs, .. } | MirInst::Compare { dst, lhs, rhs, .. } => {
                self.validate_slot(*dst)?;
                self.validate_value(module, *lhs)?;
                self.validate_value(module, *rhs)
            }
            MirInst::TypeOfValue { dst, value } => {
                self.validate_slot(*dst)?;
                self.validate_value(module, *value)
            }
            MirInst::TypeOfPlace { dst, place } => {
                self.validate_slot(*dst)?;
                self.validate_place(module, place)
            }
            MirInst::Delete { dst, place } => {
                if let Some(dst) = dst {
                    self.validate_slot(*dst)?;
                }
                self.validate_place(module, place)
            }
            MirInst::Invalidate { dst, target } | MirInst::CheckInvalidated { dst, target } => {
                self.validate_slot(*dst)?;
                self.validate_value(module, *target)
            }
            MirInst::IsInstanceOf {
                dst,
                value,
                class_name,
            } => {
                self.validate_slot(*dst)?;
                self.validate_value(module, *value)?;
                self.validate_value(module, *class_name)
            }
            MirInst::Call { dst, target, args } => {
                if let Some(dst) = dst {
                    self.validate_slot(*dst)?;
                }
                self.validate_call_target(module, target)?;
                self.validate_arg_list(module, args)
            }
            MirInst::New { dst, callee, args } => {
                if let Some(dst) = dst {
                    self.validate_slot(*dst)?;
                }
                self.validate_value(module, *callee)?;
                self.validate_arg_list(module, args)
            }
            MirInst::Eval { dst, source, .. } => {
                if let Some(dst) = dst {
                    self.validate_slot(*dst)?;
                }
                self.validate_value(module, *source)
            }
            MirInst::ChangeThis {
                dst,
                closure,
                this_obj,
            } => {
                self.validate_slot(*dst)?;
                self.validate_value(module, *closure)?;
                self.validate_value(module, *this_obj)
            }
            MirInst::LoadGlobal { dst } => self.validate_slot(*dst),
            MirInst::AddClassInfo { object, info } => {
                self.validate_value(module, *object)?;
                self.validate_value(module, *info)
            }
            MirInst::RegisterDeclaration {
                name,
                object,
                value,
                ..
            } => {
                module.require_string(*name)?;
                module.require_object(*object)?;
                if let Some(value) = value {
                    self.validate_value(module, *value)?;
                }
                Ok(())
            }
            MirInst::ApplyClassExtender {
                class_object,
                getter,
            } => {
                module.require_object(*class_object)?;
                module.require_object(*getter)
            }
            MirInst::BuildArray { dst, elements } => {
                self.validate_slot(*dst)?;
                for element in elements {
                    match element {
                        ArrayElement::Value(value) | ArrayElement::Expand(value) => {
                            self.validate_value(module, *value)?
                        }
                        ArrayElement::Hole => {}
                    }
                }
                Ok(())
            }
            MirInst::BuildDictionary { dst, entries } => {
                self.validate_slot(*dst)?;
                for entry in entries {
                    match &entry.key {
                        DictionaryKey::Direct(id) => module.require_string(*id)?,
                        DictionaryKey::Computed(value) => self.validate_value(module, *value)?,
                    }
                    self.validate_value(module, entry.value)?;
                }
                Ok(())
            }
            MirInst::BuildRegExp {
                dst,
                pattern,
                flags,
            } => {
                self.validate_slot(*dst)?;
                module.require_string(*pattern)?;
                module.require_string(*flags)
            }
            MirInst::InitDefaultArg { arg, value } => {
                if *arg as usize >= self.args.declared.len() {
                    return Err(TjsError::mir("default arg references invalid arg"));
                }
                self.validate_value(module, *value)
            }
        }
    }

    fn validate_terminator(
        &self,
        module: &MirModule,
        region_ids: &BTreeSet<ExceptionRegionId>,
        block_ids: &BTreeSet<BlockId>,
        term: &Terminator,
    ) -> Result<()> {
        match term {
            Terminator::Goto(id) => require_block(block_ids, *id),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                self.validate_condition(module, cond)?;
                require_block(block_ids, *then_block)?;
                require_block(block_ids, *else_block)
            }
            Terminator::Return { value } => {
                if let Some(value) = value {
                    self.validate_value(module, *value)?;
                }
                Ok(())
            }
            Terminator::Throw { value } => self.validate_value(module, *value),
            Terminator::LeaveTry { region, next } => {
                if !region_ids.contains(region) {
                    return Err(TjsError::mir("leave-try references invalid region"));
                }
                require_block(block_ids, *next)
            }
            Terminator::Unreachable => Ok(()),
        }
    }

    fn validate_condition(&self, module: &MirModule, cond: &Condition) -> Result<()> {
        match cond {
            Condition::Truthy(value) | Condition::Falsey(value) => {
                self.validate_value(module, *value)
            }
            Condition::ArgNeedsDefault(index) => {
                if (*index as usize) < self.args.declared.len() {
                    Ok(())
                } else {
                    Err(TjsError::mir(
                        "default arg condition references invalid arg",
                    ))
                }
            }
            Condition::Compare { lhs, rhs, .. } => {
                self.validate_value(module, *lhs)?;
                self.validate_value(module, *rhs)
            }
        }
    }

    fn validate_call_target(&self, module: &MirModule, target: &CallTarget) -> Result<()> {
        match target {
            CallTarget::Value(value) => self.validate_value(module, *value),
            CallTarget::Member { object, key, .. } => {
                self.validate_value(module, *object)?;
                self.validate_member_key(module, *key)
            }
            CallTarget::DefaultProperty { object, .. } => self.validate_value(module, *object),
        }
    }

    fn validate_arg_list(&self, module: &MirModule, args: &ArgList) -> Result<()> {
        match args {
            ArgList::Normal(args) => {
                for arg in args {
                    self.validate_value(module, *arg)?;
                }
            }
            ArgList::OmittedCallerArgs => {}
            ArgList::Expanded(args) => {
                for arg in args {
                    match arg {
                        ArgPart::Normal(value) | ArgPart::Expand(value) => {
                            self.validate_value(module, *value)?
                        }
                        ArgPart::UnnamedExpand => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_place(&self, module: &MirModule, place: &Place) -> Result<()> {
        match place {
            Place::Slot(slot) => self.validate_slot(*slot),
            Place::Member { object, key, .. } => {
                self.validate_value(module, *object)?;
                self.validate_member_key(module, *key)
            }
            Place::DefaultProperty { object, .. } => self.validate_value(module, *object),
        }
    }

    fn validate_member_key(&self, module: &MirModule, key: MemberKey) -> Result<()> {
        match key {
            MemberKey::Direct(id) => module.require_string(id),
            MemberKey::Computed(value) => self.validate_value(module, value),
        }
    }

    fn validate_value(&self, module: &MirModule, value: Value) -> Result<()> {
        match value {
            Value::Slot(slot) => self.validate_slot(slot),
            Value::Const(id) => module.require_const(id),
        }
    }

    fn validate_slot(&self, slot: SlotId) -> Result<()> {
        match slot {
            SlotId::Temp(id) => {
                if self
                    .frame
                    .temps
                    .get(id.0 as usize)
                    .is_some_and(|temp| temp.id == id)
                {
                    Ok(())
                } else {
                    Err(TjsError::mir(format!("invalid temp id {}", id.0)))
                }
            }
            SlotId::Local(id) => {
                if self
                    .frame
                    .locals
                    .get(id.0 as usize)
                    .is_some_and(|local| local.id == id)
                {
                    Ok(())
                } else {
                    Err(TjsError::mir(format!("invalid local id {}", id.0)))
                }
            }
            SlotId::Arg(index) => {
                if (index as usize) < self.args.declared.len() {
                    Ok(())
                } else {
                    Err(TjsError::mir(format!("invalid arg id {index}")))
                }
            }
            SlotId::This | SlotId::ThisProxy => Ok(()),
        }
    }
}

fn require_block(block_ids: &BTreeSet<BlockId>, id: BlockId) -> Result<()> {
    if block_ids.contains(&id) {
        Ok(())
    } else {
        Err(TjsError::mir(format!("invalid target block {}", id.0)))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextType {
    TopLevel,
    Function,
    ExprFunction,
    Property,
    PropertySetter,
    PropertyGetter,
    Class,
    SuperClassGetter,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FunctionArgs {
    pub declared: Vec<ParamDecl>,
    pub unnamed_arg_array_base: Option<u32>,
    pub collapse_base: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamDecl {
    pub name: Option<StringId>,
    pub span: Option<SpanId>,
    pub has_default: bool,
    pub collapse: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameDecl {
    pub variable_reserve_count: u32,
    pub locals: Vec<LocalDecl>,
    pub temps: Vec<TempDecl>,
}

impl Default for FrameDecl {
    fn default() -> Self {
        Self {
            variable_reserve_count: 2,
            locals: Vec::new(),
            temps: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalDecl {
    pub id: LocalId,
    pub name: Option<StringId>,
    pub binding: Option<syntax::BindingId>,
    pub span: Option<SpanId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TempDecl {
    pub id: TempId,
    pub span: Option<SpanId>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MirConst {
    Void,
    NullObject,
    Integer(i64),
    Real(f64),
    String(StringId),
    Octet(Vec<u8>),
    CodeObject(ObjectId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotId {
    Temp(TempId),
    Local(LocalId),
    Arg(u32),
    This,
    ThisProxy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Value {
    Slot(SlotId),
    Const(ConstId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    Slot(SlotId),
    Member {
        object: Value,
        key: MemberKey,
        flags: DispatchFlags,
    },
    DefaultProperty {
        object: Value,
        flags: DispatchFlags,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberKey {
    Direct(StringId),
    Computed(Value),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DispatchFlags {
    pub ensure: bool,
    pub must_exist: bool,
    pub ignore_prop: bool,
    pub hidden: bool,
}

pub const FLAGS_DEFAULT_GET: DispatchFlags = DispatchFlags {
    ensure: false,
    must_exist: false,
    ignore_prop: false,
    hidden: false,
};

pub const FLAGS_DEFAULT_SET: DispatchFlags = DispatchFlags {
    ensure: false,
    must_exist: false,
    ignore_prop: false,
    hidden: false,
};

pub const FLAGS_ENSURE_SET: DispatchFlags = DispatchFlags {
    ensure: true,
    must_exist: false,
    ignore_prop: false,
    hidden: false,
};

pub const FLAGS_IGNORE_PROP_GET: DispatchFlags = DispatchFlags {
    ensure: false,
    must_exist: false,
    ignore_prop: true,
    hidden: false,
};

pub const FLAGS_IGNORE_PROP_SET: DispatchFlags = DispatchFlags {
    ensure: true,
    must_exist: false,
    ignore_prop: true,
    hidden: false,
};

fn flags_with_ignore_prop(mut flags: DispatchFlags) -> DispatchFlags {
    flags.ignore_prop = true;
    flags
}

fn place_with_ignore_prop(place: Place) -> Place {
    match place {
        Place::Member { object, key, flags } => Place::Member {
            object,
            key,
            flags: flags_with_ignore_prop(flags),
        },
        place => place,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BasicBlock {
    pub id: BlockId,
    pub params: Vec<BlockParam>,
    pub insts: Vec<MirInst>,
    pub terminator: Terminator,
    pub source_span: Option<SpanId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockParam {
    pub name: Option<StringId>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MirInst {
    Nop,
    LoadConst {
        dst: SlotId,
        value: ConstId,
    },
    Copy {
        dst: SlotId,
        src: Value,
    },
    Clear {
        dst: SlotId,
    },
    ReadPlace {
        dst: SlotId,
        place: Place,
    },
    Assign {
        place: Place,
        value: Value,
        result: Option<SlotId>,
    },
    AssignOp {
        place: Place,
        op: BinaryOp,
        rhs: Value,
        result: Option<SlotId>,
    },
    Swap {
        left: Place,
        right: Place,
    },
    Update {
        place: Place,
        op: UpdateOp,
        result: Option<SlotId>,
        result_value: UpdateResultValue,
    },
    Unary {
        dst: SlotId,
        op: UnaryOp,
        src: Value,
    },
    Binary {
        dst: SlotId,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
    },
    Compare {
        dst: SlotId,
        op: CompareOp,
        lhs: Value,
        rhs: Value,
    },
    Convert {
        dst: SlotId,
        op: ConvertOp,
        src: Value,
    },
    ToBoolean {
        dst: SlotId,
        src: Value,
    },
    TypeOfValue {
        dst: SlotId,
        value: Value,
    },
    TypeOfPlace {
        dst: SlotId,
        place: Place,
    },
    Delete {
        dst: Option<SlotId>,
        place: Place,
    },
    Invalidate {
        dst: SlotId,
        target: Value,
    },
    CheckInvalidated {
        dst: SlotId,
        target: Value,
    },
    IsInstanceOf {
        dst: SlotId,
        value: Value,
        class_name: Value,
    },
    Call {
        dst: Option<SlotId>,
        target: CallTarget,
        args: ArgList,
    },
    New {
        dst: Option<SlotId>,
        callee: Value,
        args: ArgList,
    },
    Eval {
        dst: Option<SlotId>,
        source: Value,
        mode: EvalMode,
    },
    ChangeThis {
        dst: SlotId,
        closure: Value,
        this_obj: Value,
    },
    LoadGlobal {
        dst: SlotId,
    },
    AddClassInfo {
        object: Value,
        info: Value,
    },
    RegisterDeclaration {
        name: StringId,
        object: ObjectId,
        value: Option<Value>,
        change_this: bool,
    },
    RegisterMembers,
    ApplyClassExtender {
        class_object: ObjectId,
        getter: ObjectId,
    },
    BuildArray {
        dst: SlotId,
        elements: Vec<ArrayElement>,
    },
    BuildDictionary {
        dst: SlotId,
        entries: Vec<DictionaryEntry>,
    },
    BuildRegExp {
        dst: SlotId,
        pattern: StringId,
        flags: StringId,
    },
    InitDefaultArg {
        arg: u32,
        value: Value,
    },
    Debugger,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    LogicalNot,
    BitNot,
    Negate,
    Asc,
    Chr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConvertOp {
    Number,
    Integer,
    Real,
    String,
    Octet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    LogicalOr,
    LogicalAnd,
    BitOr,
    BitXor,
    BitAnd,
    ShiftArithmeticRight,
    ShiftLeft,
    ShiftLogicalRight,
    Add,
    Sub,
    Mod,
    Div,
    Idiv,
    Mul,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareOp {
    Equal,
    NotEqual,
    DiscernEqual,
    DiscernNotEqual,
    LessThan,
    GreaterThan,
    LessEqual,
    GreaterEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateOp {
    Inc,
    Dec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateResultValue {
    Old,
    New,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CallTarget {
    Value(Value),
    Member {
        object: Value,
        key: MemberKey,
        flags: DispatchFlags,
    },
    DefaultProperty {
        object: Value,
        flags: DispatchFlags,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArgList {
    Normal(Vec<Value>),
    OmittedCallerArgs,
    Expanded(Vec<ArgPart>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArgPart {
    Normal(Value),
    Expand(Value),
    UnnamedExpand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvalMode {
    Expression,
    Statement,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArrayElement {
    Value(Value),
    Expand(Value),
    Hole,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DictionaryEntry {
    pub key: DictionaryKey,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DictionaryKey {
    Direct(StringId),
    Computed(Value),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    Goto(BlockId),
    Branch {
        cond: Condition,
        then_block: BlockId,
        else_block: BlockId,
    },
    Return {
        value: Option<Value>,
    },
    Throw {
        value: Value,
    },
    LeaveTry {
        region: ExceptionRegionId,
        next: BlockId,
    },
    Unreachable,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Condition {
    Truthy(Value),
    Falsey(Value),
    ArgNeedsDefault(u32),
    Compare {
        op: CompareOp,
        lhs: Value,
        rhs: Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExceptionRegion {
    pub id: ExceptionRegionId,
    pub parent: Option<ExceptionRegionId>,
    pub entry: BlockId,
    pub protected_blocks: Vec<BlockId>,
    pub catch: BlockId,
    pub exception_slot: SlotId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PropertyRegistration {
    pub name: StringId,
    pub object: ObjectId,
}

pub fn lower_hir_program(
    program: &hir::Program,
    source_name: &str,
    source_text: &str,
) -> Result<MirModule> {
    let mut lowerer = Lowerer::new(program, source_name, source_text);
    let global_name = lowerer.intern_string("global");
    lowerer.record_object(ObjectId(0), ContextType::TopLevel, None);
    let mut top = ObjectBuilder::new(
        ObjectId(0),
        global_name,
        ContextType::TopLevel,
        None,
        lowerer.add_span(program.span),
    );
    top.lower_statements(&mut lowerer, &program.statements)?;
    lowerer.lower_pending_objects()?;
    lowerer.module.objects.push(top.finish());
    lowerer.module.objects.sort_by_key(|object| object.id);
    optimize_module(&mut lowerer.module);
    lowerer.module.validate()?;
    Ok(lowerer.module)
}

#[derive(Clone, Debug)]
struct BindingInfo {
    is_global: bool,
    scope_kind: hir::ScopeKind,
    kind: hir::BindingKind,
}

#[derive(Clone, Debug)]
enum ObjectJob {
    Function {
        id: ObjectId,
        decl: Box<syntax::FunctionDecl>,
        context: ContextType,
        parent: Option<ObjectId>,
    },
    Class {
        id: ObjectId,
        name: StringId,
        decl: Box<syntax::ClassDecl>,
        parent: ObjectId,
    },
    SuperClassGetter {
        id: ObjectId,
        name: StringId,
        parent: ObjectId,
        expr: syntax::Expr,
    },
}

struct Lowerer {
    module: MirModule,
    source_spans: SourceSpanMapper,
    next_object_id: u32,
    bindings: BTreeMap<syntax::BindingId, BindingInfo>,
    object_parents: BTreeMap<ObjectId, Option<ObjectId>>,
    object_contexts: BTreeMap<ObjectId, ContextType>,
    class_primary_extenders: BTreeMap<ObjectId, syntax::Expr>,
    object_jobs: VecDeque<ObjectJob>,
}

impl Lowerer {
    fn new(program: &hir::Program, source_name: &str, source_text: &str) -> Self {
        let mut bindings = BTreeMap::new();
        for binding in &program.bindings {
            let scope_kind = program
                .scopes
                .get(binding.scope.0)
                .map(|scope| scope.kind)
                .unwrap_or(hir::ScopeKind::Global);
            let is_global = scope_kind == hir::ScopeKind::Global;
            bindings.insert(
                binding.id,
                BindingInfo {
                    is_global,
                    scope_kind,
                    kind: binding.kind,
                },
            );
        }

        Self {
            module: MirModule {
                version: MIR_VERSION,
                sources: vec![SourceFile {
                    id: SourceFileId(0),
                    name: source_name.to_string(),
                    text_hash: None,
                    text: Some(source_text.to_string()),
                }],
                spans: Vec::new(),
                strings: Vec::new(),
                constants: Vec::new(),
                objects: Vec::new(),
                top_level: ObjectId(0),
            },
            source_spans: SourceSpanMapper::new(source_text),
            next_object_id: 1,
            bindings,
            object_parents: BTreeMap::new(),
            object_contexts: BTreeMap::new(),
            class_primary_extenders: BTreeMap::new(),
            object_jobs: VecDeque::new(),
        }
    }

    fn binding(&self, id: syntax::BindingId) -> Option<&BindingInfo> {
        self.bindings.get(&id)
    }

    fn ident_is_global(&self, ident: &syntax::Ident) -> bool {
        ident
            .binding
            .and_then(|id| self.binding(id))
            .is_some_and(|binding| binding.is_global)
    }

    fn ident_is_class_scoped(&self, ident: &syntax::Ident) -> bool {
        ident
            .binding
            .and_then(|id| self.binding(id))
            .is_some_and(|binding| binding.scope_kind == hir::ScopeKind::Class)
    }

    fn ident_is_property(&self, ident: &syntax::Ident) -> bool {
        ident
            .binding
            .and_then(|id| self.binding(id))
            .is_some_and(|binding| binding.kind == hir::BindingKind::Property)
    }

    fn intern_string(&mut self, text: &str) -> StringId {
        if let Some(index) = self.module.strings.iter().position(|value| value == text) {
            return StringId(index as u32);
        }
        let id = StringId(self.module.strings.len() as u32);
        self.module.strings.push(text.to_string());
        id
    }

    fn add_const(&mut self, value: MirConst) -> ConstId {
        let id = ConstId(self.module.constants.len() as u32);
        self.module.constants.push(value);
        id
    }

    fn const_void(&mut self) -> Value {
        Value::Const(self.add_const(MirConst::Void))
    }

    fn const_bool(&mut self, value: bool) -> Value {
        Value::Const(self.add_const(MirConst::Integer(i64::from(value))))
    }

    fn add_span(&mut self, span: Span) -> SpanId {
        let id = SpanId(self.module.spans.len() as u32);
        self.module
            .spans
            .push(self.source_spans.to_source_span(span));
        id
    }

    fn next_object_id(&mut self) -> ObjectId {
        let id = ObjectId(self.next_object_id);
        self.next_object_id += 1;
        id
    }

    fn record_object(&mut self, id: ObjectId, context: ContextType, parent: Option<ObjectId>) {
        self.object_contexts.insert(id, context);
        self.object_parents.insert(id, parent);
    }

    fn enclosing_super_expr(&self, object: &MirObject) -> Option<syntax::Expr> {
        if object.context == ContextType::Class
            && let Some(expr) = self.class_primary_extenders.get(&object.id)
        {
            return Some(expr.clone());
        }

        let mut current = object.parent;
        while let Some(id) = current {
            if self.object_contexts.get(&id) == Some(&ContextType::Class) {
                return self.class_primary_extenders.get(&id).cloned();
            }
            current = self.object_parents.get(&id).copied().flatten();
        }
        None
    }

    fn lower_function_object(
        &mut self,
        decl: &syntax::FunctionDecl,
        context: ContextType,
        parent: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let id = self.next_object_id();
        self.object_jobs.push_back(ObjectJob::Function {
            id,
            decl: Box::new(decl.clone()),
            context,
            parent,
        });
        Ok(id)
    }

    fn lower_super_class_getter(
        &mut self,
        name: StringId,
        parent: ObjectId,
        expr: &syntax::Expr,
    ) -> Result<ObjectId> {
        let id = self.next_object_id();
        self.object_jobs.push_back(ObjectJob::SuperClassGetter {
            id,
            name,
            parent,
            expr: expr.clone(),
        });
        Ok(id)
    }

    fn lower_pending_objects(&mut self) -> Result<()> {
        while let Some(job) = self.object_jobs.pop_front() {
            match job {
                ObjectJob::Function {
                    id,
                    decl,
                    context,
                    parent,
                } => {
                    let name = self.intern_string(
                        decl.name
                            .as_ref()
                            .map(|name| name.name.as_str())
                            .unwrap_or("(anonymous)"),
                    );
                    self.record_object(id, context, parent);
                    let mut object =
                        ObjectBuilder::new(id, name, context, parent, self.add_span(decl.span));
                    object.bind_params(self, &decl)?;
                    object.lower_stmt(self, &decl.body)?;
                    self.module.objects.push(object.finish());
                }
                ObjectJob::Class {
                    id,
                    name,
                    decl,
                    parent,
                } => {
                    self.record_object(id, ContextType::Class, Some(parent));
                    if let Some(extender) = decl.extends.first() {
                        self.class_primary_extenders.insert(id, extender.clone());
                    }
                    let mut class_object = ObjectBuilder::new(
                        id,
                        name,
                        ContextType::Class,
                        Some(parent),
                        self.add_span(decl.span),
                    );

                    for (index, extender) in decl.extends.iter().enumerate() {
                        let getter = self.lower_super_class_getter(name, id, extender)?;
                        class_object.emit(MirInst::ApplyClassExtender {
                            class_object: id,
                            getter,
                        });
                        if index == 0 {
                            class_object.object.super_class_getter = Some(getter);
                        }
                    }

                    for member in &decl.body {
                        match &member.kind {
                            syntax::StmtKind::FunctionDecl(function) => {
                                let member_id = self.lower_function_object(
                                    function,
                                    ContextType::Function,
                                    Some(id),
                                )?;
                                let member_name = self
                                    .intern_string(function.name.as_ref().map_or("", |n| &n.name));
                                class_object.object.properties.push(PropertyRegistration {
                                    name: member_name,
                                    object: member_id,
                                });
                            }
                            syntax::StmtKind::PropertyDecl(property) => {
                                let property_id = self.next_object_id();
                                let property_name = self.intern_string(&property.name.name);
                                self.record_object(property_id, ContextType::Property, Some(id));
                                let mut property_object = ObjectBuilder::new(
                                    property_id,
                                    property_name,
                                    ContextType::Property,
                                    Some(id),
                                    self.add_span(property.span),
                                );
                                class_object.populate_property_accessors(
                                    self,
                                    &mut property_object,
                                    property,
                                )?;
                                self.module.objects.push(property_object.finish());
                                class_object.object.properties.push(PropertyRegistration {
                                    name: property_name,
                                    object: property_id,
                                });
                            }
                            _ => class_object.lower_stmt(self, member)?,
                        }
                    }
                    class_object.emit(MirInst::RegisterMembers);
                    self.module.objects.push(class_object.finish());
                }
                ObjectJob::SuperClassGetter {
                    id,
                    name,
                    parent,
                    expr,
                } => {
                    self.record_object(id, ContextType::SuperClassGetter, Some(parent));
                    let mut object = ObjectBuilder::new(
                        id,
                        name,
                        ContextType::SuperClassGetter,
                        Some(parent),
                        self.add_span(expr.span),
                    );
                    let value = object.lower_expr(self, &expr)?;
                    object.terminate_return_through_regions(self, Some(value));
                    self.module.objects.push(object.finish());
                }
            }
        }
        Ok(())
    }
}

struct SourceSpanMapper {
    source_len: usize,
    utf16_offsets: Utf16Offsets,
}

enum Utf16Offsets {
    SameAsByte,
    Prefix(Vec<u32>),
}

impl SourceSpanMapper {
    fn new(source_text: &str) -> Self {
        let utf16_offsets = if source_text.is_ascii() {
            Utf16Offsets::SameAsByte
        } else {
            let mut offsets = vec![0; source_text.len() + 1];
            let mut utf16_offset = 0;
            for (byte_offset, ch) in source_text.char_indices() {
                let next_byte_offset = byte_offset + ch.len_utf8();
                offsets[byte_offset..next_byte_offset].fill(utf16_offset);
                utf16_offset += ch.len_utf16() as u32;
            }
            offsets[source_text.len()] = utf16_offset;
            Utf16Offsets::Prefix(offsets)
        };

        Self {
            source_len: source_text.len(),
            utf16_offsets,
        }
    }

    fn to_source_span(&self, span: Span) -> SourceSpan {
        let start = span.start.min(self.source_len);
        let end = span.end.min(self.source_len);
        SourceSpan {
            file: SourceFileId(0),
            byte_start: start as u32,
            byte_end: end as u32,
            utf16_start: self.utf16_offset(start),
            utf16_end: self.utf16_offset(end),
        }
    }

    fn utf16_offset(&self, byte_offset: usize) -> u32 {
        match &self.utf16_offsets {
            Utf16Offsets::SameAsByte => byte_offset as u32,
            Utf16Offsets::Prefix(offsets) => offsets[byte_offset],
        }
    }
}

struct ObjectBuilder {
    object: MirObject,
    current: BlockId,
    binding_slots: BTreeMap<syntax::BindingId, SlotId>,
    force_global_context: bool,
    with_stack: Vec<Value>,
    control_stack: Vec<ControlFrame>,
    active_regions: Vec<ExceptionRegionId>,
}

#[derive(Clone, Copy, Debug)]
enum ControlFrame {
    Loop {
        break_block: BlockId,
        continue_block: BlockId,
    },
    Switch {
        break_block: BlockId,
    },
}

#[derive(Clone, Debug)]
enum PendingExit {
    Goto(BlockId),
    Return(Option<Value>),
}

enum StmtTask<'a> {
    Stmt(&'a syntax::Stmt),
    StartBlock(BlockId),
    GotoIfOpen(BlockId),
    LeaveTryIfOpen {
        region: ExceptionRegionId,
        next: BlockId,
    },
    SetTerminator(Terminator),
    ControlPush(ControlFrame),
    ControlPop,
    ActiveRegionPop,
    WithPop,
    DoWhileCondition {
        condition: &'a syntax::Expr,
        body_block: BlockId,
        after_block: BlockId,
    },
    ForStep {
        step: Option<&'a syntax::Expr>,
        cond_block: BlockId,
    },
}

enum ExprTask<'a> {
    Expr(&'a syntax::Expr),
    PushValue(Value),
    DiscardValue,
    StartBlock(BlockId),
    GotoIfOpen(BlockId),
    ReadPlaceToValue,
    WritePlace(&'a syntax::Expr),
    ApplyIgnorePropToPlace,
    MemberPlaceAfterObject {
        property: &'a str,
        flags: DispatchFlags,
    },
    DefaultPropertyPlaceAfterObject {
        flags: DispatchFlags,
    },
    IndexPlaceAfterObject {
        index: &'a syntax::Expr,
        flags: DispatchFlags,
    },
    IndexPlaceAfterIndex {
        object: Value,
        flags: DispatchFlags,
    },
    CallTarget(&'a syntax::Expr),
    CallTargetValueAfter,
    CallTargetMemberAfterObject {
        property: &'a str,
        flags: DispatchFlags,
    },
    CallTargetDefaultPropertyAfterObject {
        flags: DispatchFlags,
    },
    CallTargetIndexAfterObject {
        index: &'a syntax::Expr,
        flags: DispatchFlags,
    },
    CallTargetIndexAfterIndex {
        object: Value,
        flags: DispatchFlags,
    },
    CallArgs(&'a [syntax::CallArg]),
    BuildCallArgs {
        args: &'a [syntax::CallArg],
        value_count: usize,
        saw_expanded: bool,
    },
    AfterUnary(syntax::UnaryOp),
    AfterBinary(syntax::BinaryOp),
    AfterInContextOf,
    AfterInstanceOf,
    AfterAssignment(syntax::AssignOp),
    AfterSwap,
    AfterDelete,
    AfterPrefixUpdate(syntax::UnaryOp),
    AfterPostfixUpdate(syntax::UnaryOp),
    AfterPostfixEval,
    AfterTypeOfValue,
    AfterTypeOfPlace,
    AfterPropAccess,
    AfterShortCircuitLhs {
        op: syntax::BinaryOp,
        rhs: &'a syntax::Expr,
    },
    EmitShortCircuitShortcut {
        block: BlockId,
        result: SlotId,
        shortcut: Value,
        done_block: BlockId,
    },
    AfterShortCircuitRhs {
        result: SlotId,
        done_block: BlockId,
    },
    AfterIfOperatorCond {
        lhs: &'a syntax::Expr,
    },
    AfterConditionalCond {
        then_expr: &'a syntax::Expr,
        else_expr: &'a syntax::Expr,
        result: SlotId,
    },
    AfterConditionalBranch {
        result: SlotId,
        done_block: BlockId,
    },
    AfterCallArgs,
    AfterNewCallee {
        args: &'a [syntax::CallArg],
    },
    AfterNewArgs {
        callee: Value,
    },
    BuildArray {
        elements: &'a [syntax::ArrayElement],
        value_count: usize,
    },
    BuildDictionary {
        plan: Vec<DictionaryKeyPlan>,
        value_count: usize,
    },
    FinishComma {
        count: usize,
    },
}

struct ExprLoweringState<'a> {
    tasks: Vec<ExprTask<'a>>,
    values: Vec<Value>,
    places: Vec<Place>,
    call_targets: Vec<CallTarget>,
    arg_lists: Vec<ArgList>,
}

impl<'a> ExprLoweringState<'a> {
    fn new(expr: &'a syntax::Expr) -> Self {
        Self {
            tasks: vec![ExprTask::Expr(expr)],
            values: Vec::new(),
            places: Vec::new(),
            call_targets: Vec::new(),
            arg_lists: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum DictionaryKeyPlan {
    Direct(StringId),
    Computed,
}

impl ObjectBuilder {
    fn new(
        id: ObjectId,
        name: StringId,
        context: ContextType,
        parent: Option<ObjectId>,
        source_span: SpanId,
    ) -> Self {
        let entry = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            insts: Vec::new(),
            terminator: Terminator::Unreachable,
            source_span: Some(source_span),
        };
        Self {
            object: MirObject {
                id,
                name,
                context,
                parent,
                prop_getter: None,
                prop_setter: None,
                super_class_getter: None,
                args: FunctionArgs::default(),
                frame: FrameDecl::default(),
                properties: Vec::new(),
                blocks: vec![entry],
                entry: BlockId(0),
                exception_regions: Vec::new(),
                source_span: Some(source_span),
            },
            current: BlockId(0),
            binding_slots: BTreeMap::new(),
            force_global_context: false,
            with_stack: Vec::new(),
            control_stack: Vec::new(),
            active_regions: Vec::new(),
        }
    }

    fn finish(mut self) -> MirObject {
        if self.current_open() {
            self.set_terminator(Terminator::Return { value: None });
        }
        self.object
    }

    fn bind_params(&mut self, lowerer: &mut Lowerer, decl: &syntax::FunctionDecl) -> Result<()> {
        for (index, param) in decl.params.iter().enumerate() {
            let name = param
                .name
                .as_ref()
                .map(|name| lowerer.intern_string(&name.name));
            let span = Some(lowerer.add_span(param.span));
            self.object.args.declared.push(ParamDecl {
                name,
                span,
                has_default: param.default.is_some(),
                collapse: param.collapse,
            });
            if let Some(ident) = &param.name
                && let Some(binding) = ident.binding
            {
                self.binding_slots
                    .insert(binding, SlotId::Arg(index as u32));
            }
            if param.collapse {
                if param.name.is_some() {
                    self.object.args.collapse_base = Some(index as u32);
                } else {
                    self.object.args.unnamed_arg_array_base = Some(index as u32);
                }
            }
        }

        for (index, param) in decl.params.iter().enumerate() {
            if let Some(default) = &param.default {
                let default_block = self.new_block(Some(lowerer.add_span(default.span)));
                let done_block = self.new_block(None);
                self.set_terminator(Terminator::Branch {
                    cond: Condition::ArgNeedsDefault(index as u32),
                    then_block: default_block,
                    else_block: done_block,
                });

                self.start_block(default_block);
                let value = self.lower_expr(lowerer, default)?;
                self.emit(MirInst::InitDefaultArg {
                    arg: index as u32,
                    value,
                });
                self.set_terminator(Terminator::Goto(done_block));
                self.start_block(done_block);
            }
        }
        Ok(())
    }

    fn current_block_mut(&mut self) -> &mut BasicBlock {
        let index = self
            .object
            .blocks
            .iter()
            .position(|block| block.id == self.current)
            .expect("current block exists");
        &mut self.object.blocks[index]
    }

    fn current_open(&self) -> bool {
        self.object
            .blocks
            .iter()
            .find(|block| block.id == self.current)
            .is_some_and(|block| matches!(block.terminator, Terminator::Unreachable))
    }

    fn ensure_open(&mut self) {
        if !self.current_open() {
            let block = self.new_block(None);
            self.current = block;
        }
    }

    fn emit(&mut self, inst: MirInst) {
        self.ensure_open();
        self.current_block_mut().insts.push(inst);
    }

    fn set_terminator(&mut self, terminator: Terminator) {
        if self.current_open() {
            self.current_block_mut().terminator = terminator;
        }
    }

    fn new_block(&mut self, source_span: Option<SpanId>) -> BlockId {
        let regions = self.active_regions.clone();
        self.new_block_in_regions(source_span, &regions)
    }

    fn new_block_in_regions(
        &mut self,
        source_span: Option<SpanId>,
        regions: &[ExceptionRegionId],
    ) -> BlockId {
        let id = BlockId(self.object.blocks.len() as u32);
        self.object.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            insts: Vec::new(),
            terminator: Terminator::Unreachable,
            source_span,
        });
        self.mark_block_for_regions(id, regions);
        id
    }

    fn start_block(&mut self, id: BlockId) {
        self.current = id;
    }

    fn mark_block_for_active_regions(&mut self, id: BlockId) {
        let regions = self.active_regions.clone();
        self.mark_block_for_regions(id, &regions);
    }

    fn mark_block_for_regions(&mut self, id: BlockId, regions: &[ExceptionRegionId]) {
        for region in regions {
            let region = self
                .object
                .exception_regions
                .iter_mut()
                .find(|candidate| candidate.id == *region)
                .expect("active region exists");
            if !region.protected_blocks.contains(&id) {
                region.protected_blocks.push(id);
            }
        }
    }

    fn temp(&mut self) -> SlotId {
        let id = TempId(self.object.frame.temps.len() as u32);
        self.object.frame.temps.push(TempDecl { id, span: None });
        SlotId::Temp(id)
    }

    fn global_value(&mut self) -> Value {
        let dst = self.temp();
        self.emit(MirInst::LoadGlobal { dst });
        Value::Slot(dst)
    }

    fn local(
        &mut self,
        lowerer: &mut Lowerer,
        binding: Option<syntax::BindingId>,
        name: Option<&str>,
        span: Span,
    ) -> SlotId {
        if let Some(binding) = binding
            && let Some(slot) = self.binding_slots.get(&binding)
        {
            return *slot;
        }

        let id = LocalId(self.object.frame.locals.len() as u32);
        let name_id = name.map(|name| lowerer.intern_string(name));
        let span = Some(lowerer.add_span(span));
        let slot = SlotId::Local(id);
        self.object.frame.locals.push(LocalDecl {
            id,
            name: name_id,
            binding,
            span,
        });
        if let Some(binding) = binding {
            self.binding_slots.insert(binding, slot);
        }
        slot
    }

    fn read_place(&mut self, place: Place) -> Value {
        let dst = self.temp();
        self.emit(MirInst::ReadPlace { dst, place });
        Value::Slot(dst)
    }

    fn copy_to_temp(&mut self, value: Value) -> Value {
        let dst = self.temp();
        self.emit(MirInst::Copy { dst, src: value });
        Value::Slot(dst)
    }

    fn lower_statements(
        &mut self,
        lowerer: &mut Lowerer,
        statements: &[syntax::Stmt],
    ) -> Result<()> {
        let mut tasks = Vec::new();
        push_stmt_tasks(&mut tasks, statements);
        self.run_stmt_tasks(lowerer, &mut tasks)
    }

    fn lower_stmt(&mut self, lowerer: &mut Lowerer, statement: &syntax::Stmt) -> Result<()> {
        let mut tasks = vec![StmtTask::Stmt(statement)];
        self.run_stmt_tasks(lowerer, &mut tasks)
    }

    fn run_stmt_tasks<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        tasks: &mut Vec<StmtTask<'a>>,
    ) -> Result<()> {
        while let Some(task) = tasks.pop() {
            match task {
                StmtTask::Stmt(statement) => self.push_stmt_task(lowerer, statement, tasks)?,
                StmtTask::StartBlock(block) => self.start_block(block),
                StmtTask::GotoIfOpen(target) => {
                    if self.current_open() {
                        self.set_terminator(Terminator::Goto(target));
                    }
                }
                StmtTask::LeaveTryIfOpen { region, next } => {
                    if self.current_open() {
                        self.set_terminator(Terminator::LeaveTry { region, next });
                    }
                }
                StmtTask::SetTerminator(terminator) => self.set_terminator(terminator),
                StmtTask::ControlPush(frame) => self.control_stack.push(frame),
                StmtTask::ControlPop => {
                    self.control_stack.pop();
                }
                StmtTask::ActiveRegionPop => {
                    self.active_regions.pop();
                }
                StmtTask::WithPop => {
                    self.with_stack.pop();
                }
                StmtTask::DoWhileCondition {
                    condition,
                    body_block,
                    after_block,
                } => {
                    let cond = self.lower_expr(lowerer, condition)?;
                    self.set_terminator(Terminator::Branch {
                        cond: Condition::Truthy(cond),
                        then_block: body_block,
                        else_block: after_block,
                    });
                }
                StmtTask::ForStep { step, cond_block } => {
                    if let Some(step) = step {
                        self.lower_expr(lowerer, step)?;
                    }
                    self.set_terminator(Terminator::Goto(cond_block));
                }
            }
        }
        Ok(())
    }

    fn push_stmt_task<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        statement: &'a syntax::Stmt,
        tasks: &mut Vec<StmtTask<'a>>,
    ) -> Result<()> {
        match &statement.kind {
            syntax::StmtKind::Empty => {}
            syntax::StmtKind::Block(statements) => push_stmt_tasks(tasks, statements),
            syntax::StmtKind::Expr(expr) => {
                self.lower_expr(lowerer, expr)?;
            }
            syntax::StmtKind::Var { declarations, .. } => {
                for decl in declarations {
                    self.lower_var_decl(lowerer, decl)?;
                }
            }
            syntax::StmtKind::FunctionDecl(decl) => {
                self.lower_function_decl(lowerer, decl)?;
            }
            syntax::StmtKind::ClassDecl(decl) => {
                self.lower_class_decl(lowerer, decl)?;
            }
            syntax::StmtKind::PropertyDecl(decl) => {
                self.lower_property_decl(lowerer, decl)?;
            }
            syntax::StmtKind::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|expr| self.lower_expr(lowerer, expr))
                    .transpose()?;
                self.terminate_return_through_regions(lowerer, value);
            }
            syntax::StmtKind::Throw(expr) => {
                let value = self.lower_expr(lowerer, expr)?;
                self.set_terminator(Terminator::Throw { value });
            }
            syntax::StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let cond = self.lower_expr(lowerer, condition)?;
                let then_block = self.new_block(Some(lowerer.add_span(then_branch.span)));
                let else_block =
                    self.new_block(else_branch.as_ref().map(|stmt| lowerer.add_span(stmt.span)));
                let join_block = self.new_block(None);
                self.set_terminator(Terminator::Branch {
                    cond: Condition::Truthy(cond),
                    then_block,
                    else_block,
                });

                tasks.push(StmtTask::StartBlock(join_block));
                tasks.push(StmtTask::GotoIfOpen(join_block));
                if let Some(else_branch) = else_branch {
                    tasks.push(StmtTask::Stmt(else_branch.as_ref()));
                }
                tasks.push(StmtTask::StartBlock(else_block));
                tasks.push(StmtTask::GotoIfOpen(join_block));
                tasks.push(StmtTask::Stmt(then_branch.as_ref()));
                tasks.push(StmtTask::StartBlock(then_block));
            }
            syntax::StmtKind::While { condition, body } => {
                let cond_block = self.new_block(Some(lowerer.add_span(condition.span)));
                let body_block = self.new_block(Some(lowerer.add_span(body.span)));
                let after_block = self.new_block(None);
                self.set_terminator(Terminator::Goto(cond_block));

                self.start_block(cond_block);
                let cond = self.lower_expr(lowerer, condition)?;
                self.set_terminator(Terminator::Branch {
                    cond: Condition::Truthy(cond),
                    then_block: body_block,
                    else_block: after_block,
                });

                tasks.push(StmtTask::StartBlock(after_block));
                tasks.push(StmtTask::ControlPop);
                tasks.push(StmtTask::GotoIfOpen(cond_block));
                tasks.push(StmtTask::Stmt(body.as_ref()));
                tasks.push(StmtTask::StartBlock(body_block));
                tasks.push(StmtTask::ControlPush(ControlFrame::Loop {
                    break_block: after_block,
                    continue_block: cond_block,
                }));
            }
            syntax::StmtKind::DoWhile { body, condition } => {
                let body_block = self.new_block(Some(lowerer.add_span(body.span)));
                let cond_block = self.new_block(Some(lowerer.add_span(condition.span)));
                let after_block = self.new_block(None);
                self.set_terminator(Terminator::Goto(body_block));

                tasks.push(StmtTask::StartBlock(after_block));
                tasks.push(StmtTask::DoWhileCondition {
                    condition,
                    body_block,
                    after_block,
                });
                tasks.push(StmtTask::StartBlock(cond_block));
                tasks.push(StmtTask::ControlPop);
                tasks.push(StmtTask::GotoIfOpen(cond_block));
                tasks.push(StmtTask::Stmt(body.as_ref()));
                tasks.push(StmtTask::StartBlock(body_block));
                tasks.push(StmtTask::ControlPush(ControlFrame::Loop {
                    break_block: after_block,
                    continue_block: cond_block,
                }));
            }
            syntax::StmtKind::For {
                init,
                condition,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.lower_for_init(lowerer, init)?;
                }

                let cond_block =
                    self.new_block(condition.as_ref().map(|expr| lowerer.add_span(expr.span)));
                let body_block = self.new_block(Some(lowerer.add_span(body.span)));
                let step_block =
                    self.new_block(step.as_ref().map(|expr| lowerer.add_span(expr.span)));
                let after_block = self.new_block(None);
                self.set_terminator(Terminator::Goto(cond_block));

                self.start_block(cond_block);
                if let Some(condition) = condition {
                    let cond = self.lower_expr(lowerer, condition)?;
                    self.set_terminator(Terminator::Branch {
                        cond: Condition::Truthy(cond),
                        then_block: body_block,
                        else_block: after_block,
                    });
                } else {
                    self.set_terminator(Terminator::Goto(body_block));
                }

                tasks.push(StmtTask::StartBlock(after_block));
                tasks.push(StmtTask::ForStep {
                    step: step.as_ref(),
                    cond_block,
                });
                tasks.push(StmtTask::StartBlock(step_block));
                tasks.push(StmtTask::ControlPop);
                tasks.push(StmtTask::GotoIfOpen(step_block));
                tasks.push(StmtTask::Stmt(body.as_ref()));
                tasks.push(StmtTask::StartBlock(body_block));
                tasks.push(StmtTask::ControlPush(ControlFrame::Loop {
                    break_block: after_block,
                    continue_block: step_block,
                }));
            }
            syntax::StmtKind::With { object, body } => {
                let value = self.lower_expr(lowerer, object)?;
                let value = self.copy_to_temp(value);
                self.with_stack.push(value);
                tasks.push(StmtTask::WithPop);
                tasks.push(StmtTask::Stmt(body.as_ref()));
            }
            syntax::StmtKind::Break => self.lower_break(lowerer)?,
            syntax::StmtKind::Continue => self.lower_continue(lowerer)?,
            syntax::StmtKind::Try { body, catch } => {
                self.ensure_open();
                let body_block = self.new_block(Some(lowerer.add_span(body.span)));
                let catch_block = self.new_block(catch.as_ref().map(|c| lowerer.add_span(c.span)));
                let after_block = self.new_block(None);
                self.set_terminator(Terminator::Goto(body_block));

                let exception_slot = if let Some(catch) = catch
                    && let Some(binding) = &catch.binding
                {
                    self.local(lowerer, binding.binding, Some(&binding.name), catch.span)
                } else {
                    self.temp()
                };

                let region = ExceptionRegionId(self.object.exception_regions.len() as u32);
                let parent = self.active_regions.last().copied();
                self.object.exception_regions.push(ExceptionRegion {
                    id: region,
                    parent,
                    entry: body_block,
                    protected_blocks: Vec::new(),
                    catch: catch_block,
                    exception_slot,
                });
                self.active_regions.push(region);
                self.mark_block_for_active_regions(body_block);

                tasks.push(StmtTask::StartBlock(after_block));
                tasks.push(StmtTask::GotoIfOpen(after_block));
                if let Some(catch) = catch {
                    tasks.push(StmtTask::Stmt(catch.body.as_ref()));
                } else {
                    tasks.push(StmtTask::SetTerminator(Terminator::Unreachable));
                }
                tasks.push(StmtTask::StartBlock(catch_block));
                tasks.push(StmtTask::ActiveRegionPop);
                tasks.push(StmtTask::LeaveTryIfOpen {
                    region,
                    next: after_block,
                });
                tasks.push(StmtTask::Stmt(body.as_ref()));
                tasks.push(StmtTask::StartBlock(body_block));
            }
            syntax::StmtKind::Switch {
                discriminant,
                cases,
            } => {
                let discriminant_value = self.lower_expr(lowerer, discriminant)?;
                let discriminant = self.copy_to_temp(discriminant_value);
                let after_block = self.new_block(None);
                if cases.is_empty() {
                    self.set_terminator(Terminator::Goto(after_block));
                    self.start_block(after_block);
                    return Ok(());
                }

                let body_blocks = cases
                    .iter()
                    .map(|case| self.new_block(Some(lowerer.add_span(case.span))))
                    .collect::<Vec<_>>();
                let test_blocks = cases
                    .iter()
                    .filter(|case| case.test.is_some())
                    .map(|case| self.new_block(Some(lowerer.add_span(case.span))))
                    .collect::<Vec<_>>();
                let default_body = cases
                    .iter()
                    .position(|case| case.test.is_none())
                    .map(|index| body_blocks[index]);
                let fallback = default_body.unwrap_or(after_block);

                let first_test = test_blocks.first().copied().unwrap_or(fallback);
                self.set_terminator(Terminator::Goto(first_test));

                let mut test_iter = test_blocks.iter().copied().peekable();
                for (case_index, case) in cases.iter().enumerate() {
                    let Some(test) = &case.test else {
                        continue;
                    };
                    let test_block = test_iter.next().expect("test block for case");
                    let next_test = test_iter.peek().copied().unwrap_or(fallback);
                    self.start_block(test_block);
                    let value = self.lower_expr(lowerer, test)?;
                    self.set_terminator(Terminator::Branch {
                        cond: Condition::Compare {
                            op: CompareOp::Equal,
                            lhs: discriminant,
                            rhs: value,
                        },
                        then_block: body_blocks[case_index],
                        else_block: next_test,
                    });
                }

                tasks.push(StmtTask::StartBlock(after_block));
                tasks.push(StmtTask::ControlPop);
                for (index, case) in cases.iter().enumerate().rev() {
                    let next = body_blocks.get(index + 1).copied().unwrap_or(after_block);
                    tasks.push(StmtTask::GotoIfOpen(next));
                    push_stmt_tasks(tasks, &case.body);
                    tasks.push(StmtTask::StartBlock(body_blocks[index]));
                }
                tasks.push(StmtTask::ControlPush(ControlFrame::Switch {
                    break_block: after_block,
                }));
            }
            syntax::StmtKind::Case { test } => {
                if let Some(test) = test {
                    self.lower_expr(lowerer, test)?;
                }
            }
            syntax::StmtKind::Debugger => self.emit(MirInst::Debugger),
        }
        Ok(())
    }

    fn lower_var_decl(&mut self, lowerer: &mut Lowerer, decl: &syntax::VarDecl) -> Result<()> {
        let value = if let Some(initializer) = &decl.initializer {
            self.lower_expr(lowerer, initializer)?
        } else {
            lowerer.const_void()
        };
        let place = self.ident_declaration_place(lowerer, &decl.name, decl.span);
        match &place {
            Place::Slot(slot) => {
                if decl.initializer.is_some() {
                    self.emit(MirInst::Copy {
                        dst: *slot,
                        src: value,
                    });
                } else {
                    self.emit(MirInst::Clear { dst: *slot });
                }
            }
            _ => self.emit(MirInst::Assign {
                place,
                value,
                result: None,
            }),
        }
        Ok(())
    }

    fn lower_function_decl(
        &mut self,
        lowerer: &mut Lowerer,
        decl: &syntax::FunctionDecl,
    ) -> Result<()> {
        let id =
            lowerer.lower_function_object(decl, ContextType::Function, Some(self.object.id))?;
        let value = Value::Const(lowerer.add_const(MirConst::CodeObject(id)));
        if let Some(name) = &decl.name {
            let name_id = lowerer.intern_string(&name.name);
            let place = self.ident_declaration_place(lowerer, name, decl.span);
            match &place {
                Place::Slot(slot) => self.emit(MirInst::Copy {
                    dst: *slot,
                    src: value,
                }),
                _ => self.emit(MirInst::Assign {
                    place,
                    value,
                    result: None,
                }),
            }
            self.emit(MirInst::RegisterDeclaration {
                name: name_id,
                object: id,
                value: Some(value),
                change_this: true,
            });
        }
        Ok(())
    }

    fn lower_class_decl(&mut self, lowerer: &mut Lowerer, decl: &syntax::ClassDecl) -> Result<()> {
        let class_id = lowerer.next_object_id();
        let class_name = lowerer.intern_string(&decl.name.name);
        lowerer.object_jobs.push_back(ObjectJob::Class {
            id: class_id,
            name: class_name,
            decl: Box::new(decl.clone()),
            parent: self.object.id,
        });

        let value = Value::Const(lowerer.add_const(MirConst::CodeObject(class_id)));
        let place = self.ident_declaration_place(lowerer, &decl.name, decl.span);
        match &place {
            Place::Slot(slot) => self.emit(MirInst::Copy {
                dst: *slot,
                src: value,
            }),
            _ => self.emit(MirInst::Assign {
                place,
                value,
                result: None,
            }),
        }
        self.emit(MirInst::RegisterDeclaration {
            name: class_name,
            object: class_id,
            value: Some(value),
            change_this: false,
        });
        Ok(())
    }

    fn lower_property_decl(
        &mut self,
        lowerer: &mut Lowerer,
        decl: &syntax::PropertyDecl,
    ) -> Result<()> {
        let property_id = lowerer.next_object_id();
        let property_name = lowerer.intern_string(&decl.name.name);
        lowerer.record_object(property_id, ContextType::Property, Some(self.object.id));
        let mut property_object = ObjectBuilder::new(
            property_id,
            property_name,
            ContextType::Property,
            Some(self.object.id),
            lowerer.add_span(decl.span),
        );
        self.populate_property_accessors(lowerer, &mut property_object, decl)?;
        lowerer.module.objects.push(property_object.finish());

        let value = Value::Const(lowerer.add_const(MirConst::CodeObject(property_id)));
        let place = self.ident_declaration_place(lowerer, &decl.name, decl.span);
        match &place {
            Place::Slot(slot) => self.emit(MirInst::Copy {
                dst: *slot,
                src: value,
            }),
            _ => self.emit(MirInst::Assign {
                place,
                value,
                result: None,
            }),
        }
        self.emit(MirInst::RegisterDeclaration {
            name: property_name,
            object: property_id,
            value: Some(value),
            change_this: true,
        });
        Ok(())
    }

    fn populate_property_accessors(
        &mut self,
        lowerer: &mut Lowerer,
        property_object: &mut ObjectBuilder,
        decl: &syntax::PropertyDecl,
    ) -> Result<()> {
        if let Some(getter) = &decl.getter {
            let getter_id = lowerer.lower_function_object(
                getter,
                ContextType::PropertyGetter,
                Some(property_object.object.id),
            )?;
            property_object.object.prop_getter = Some(getter_id);
        }
        if let Some(setter) = &decl.setter {
            let setter_id = lowerer.lower_function_object(
                setter,
                ContextType::PropertySetter,
                Some(property_object.object.id),
            )?;
            property_object.object.prop_setter = Some(setter_id);
        }
        Ok(())
    }

    fn lower_for_init(&mut self, lowerer: &mut Lowerer, init: &syntax::ForInit) -> Result<()> {
        match init {
            syntax::ForInit::Var { declarations, .. } => {
                for declaration in declarations {
                    self.lower_var_decl(lowerer, declaration)?;
                }
            }
            syntax::ForInit::Expr(expr) => {
                self.lower_expr(lowerer, expr)?;
            }
        }
        Ok(())
    }

    fn lower_break(&mut self, lowerer: &mut Lowerer) -> Result<()> {
        let Some(target) =
            self.control_stack
                .iter()
                .rev()
                .map(|frame| match frame {
                    ControlFrame::Loop { break_block, .. }
                    | ControlFrame::Switch { break_block } => *break_block,
                })
                .next()
        else {
            return Err(TjsError::mir("break statement has no target"));
        };
        self.terminate_exit_through_regions(lowerer, PendingExit::Goto(target));
        Ok(())
    }

    fn lower_continue(&mut self, lowerer: &mut Lowerer) -> Result<()> {
        let Some(target) = self
            .control_stack
            .iter()
            .rev()
            .find_map(|frame| match frame {
                ControlFrame::Loop { continue_block, .. } => Some(*continue_block),
                ControlFrame::Switch { .. } => None,
            })
        else {
            return Err(TjsError::mir("continue statement has no target"));
        };
        self.terminate_exit_through_regions(lowerer, PendingExit::Goto(target));
        Ok(())
    }

    fn terminate_return_through_regions(&mut self, lowerer: &mut Lowerer, value: Option<Value>) {
        self.terminate_exit_through_regions(lowerer, PendingExit::Return(value));
    }

    fn terminate_exit_through_regions(&mut self, lowerer: &mut Lowerer, exit: PendingExit) {
        let keep_count = match exit {
            PendingExit::Goto(target) => self.active_region_prefix_for_target(target),
            PendingExit::Return(_) => 0,
        };

        if keep_count == self.active_regions.len() {
            match exit {
                PendingExit::Goto(block) => self.set_terminator(Terminator::Goto(block)),
                PendingExit::Return(value) => self.set_terminator(Terminator::Return { value }),
            }
            return;
        }

        let regions = self.active_regions.clone();
        let mut region_count = regions.len();
        let mut next = self.new_block_in_regions(None, &regions[..region_count - 1]);
        self.set_terminator(Terminator::LeaveTry {
            region: regions[region_count - 1],
            next,
        });
        region_count -= 1;
        while region_count > keep_count {
            self.start_block(next);
            let after = self.new_block_in_regions(None, &regions[..region_count - 1]);
            self.set_terminator(Terminator::LeaveTry {
                region: regions[region_count - 1],
                next: after,
            });
            next = after;
            region_count -= 1;
        }
        self.start_block(next);
        match exit {
            PendingExit::Goto(block) => self.set_terminator(Terminator::Goto(block)),
            PendingExit::Return(value) => self.set_terminator(Terminator::Return { value }),
        }
        let active_regions = self.active_regions.clone();
        let dead =
            self.new_block_in_regions(Some(lowerer.add_span(Span::empty(0))), &active_regions);
        self.start_block(dead);
    }

    fn active_region_prefix_for_target(&self, target: BlockId) -> usize {
        self.active_regions
            .iter()
            .take_while(|region| self.region_protects_block(**region, target))
            .count()
    }

    fn region_protects_block(&self, region: ExceptionRegionId, target: BlockId) -> bool {
        self.object
            .exception_regions
            .iter()
            .find(|candidate| candidate.id == region)
            .is_some_and(|region| region.protected_blocks.contains(&target))
    }

    fn lower_expr(&mut self, lowerer: &mut Lowerer, expr: &syntax::Expr) -> Result<Value> {
        let mut state = ExprLoweringState::new(expr);
        while let Some(task) = state.tasks.pop() {
            self.run_expr_task(lowerer, task, &mut state)?;
        }
        pop_value(&mut state.values)
    }

    fn lower_expr_in_global_context(
        &mut self,
        lowerer: &mut Lowerer,
        expr: &syntax::Expr,
    ) -> Result<Value> {
        let previous = self.force_global_context;
        self.force_global_context = true;
        let result = self.lower_expr(lowerer, expr);
        self.force_global_context = previous;
        result
    }

    fn lower_super_expr(&mut self, lowerer: &mut Lowerer) -> Result<Value> {
        let Some(expr) = lowerer.enclosing_super_expr(&self.object) else {
            return Err(TjsError::mir(
                "cannot get super outside a class with extends",
            ));
        };
        self.lower_expr_in_global_context(lowerer, &expr)
    }

    fn run_expr_task<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        task: ExprTask<'a>,
        state: &mut ExprLoweringState<'a>,
    ) -> Result<()> {
        let ExprLoweringState {
            tasks,
            values,
            places,
            call_targets,
            arg_lists,
        } = state;

        match task {
            ExprTask::Expr(expr) => self.push_expr_task(lowerer, expr, tasks, values, places)?,
            ExprTask::PushValue(value) => values.push(value),
            ExprTask::DiscardValue => {
                pop_value(values)?;
            }
            ExprTask::StartBlock(block) => self.start_block(block),
            ExprTask::GotoIfOpen(target) => {
                if self.current_open() {
                    self.set_terminator(Terminator::Goto(target));
                }
            }
            ExprTask::ReadPlaceToValue => {
                let place = pop_place(places)?;
                values.push(self.read_place(place));
            }
            ExprTask::WritePlace(expr) => {
                self.push_write_place_task(lowerer, expr, tasks, places)?;
            }
            ExprTask::ApplyIgnorePropToPlace => {
                let place = pop_place(places)?;
                places.push(place_with_ignore_prop(place));
            }
            ExprTask::MemberPlaceAfterObject { property, flags } => {
                let object = pop_value(values)?;
                places.push(Place::Member {
                    object,
                    key: MemberKey::Direct(lowerer.intern_string(property)),
                    flags,
                });
            }
            ExprTask::DefaultPropertyPlaceAfterObject { flags } => {
                let object = pop_value(values)?;
                places.push(Place::DefaultProperty { object, flags });
            }
            ExprTask::IndexPlaceAfterObject { index, flags } => {
                let object = pop_value(values)?;
                tasks.push(ExprTask::IndexPlaceAfterIndex { object, flags });
                tasks.push(ExprTask::Expr(index));
            }
            ExprTask::IndexPlaceAfterIndex { object, flags } => {
                let index = pop_value(values)?;
                places.push(Place::Member {
                    object,
                    key: MemberKey::Computed(index),
                    flags,
                });
            }
            ExprTask::CallTarget(callee) => {
                self.push_call_target_task(lowerer, callee, tasks, call_targets)?;
            }
            ExprTask::CallTargetValueAfter => {
                call_targets.push(CallTarget::Value(pop_value(values)?));
            }
            ExprTask::CallTargetMemberAfterObject { property, flags } => {
                let object = pop_value(values)?;
                call_targets.push(CallTarget::Member {
                    object,
                    key: MemberKey::Direct(lowerer.intern_string(property)),
                    flags,
                });
            }
            ExprTask::CallTargetDefaultPropertyAfterObject { flags } => {
                let object = pop_value(values)?;
                call_targets.push(CallTarget::DefaultProperty { object, flags });
            }
            ExprTask::CallTargetIndexAfterObject { index, flags } => {
                let object = pop_value(values)?;
                tasks.push(ExprTask::CallTargetIndexAfterIndex { object, flags });
                tasks.push(ExprTask::Expr(index));
            }
            ExprTask::CallTargetIndexAfterIndex { object, flags } => {
                let index = pop_value(values)?;
                call_targets.push(CallTarget::Member {
                    object,
                    key: MemberKey::Computed(index),
                    flags,
                });
            }
            ExprTask::CallArgs(args) => {
                self.push_call_args_task(args, tasks, arg_lists);
            }
            ExprTask::BuildCallArgs {
                args,
                value_count,
                saw_expanded,
            } => {
                let lowered = split_values(values, value_count)?;
                if saw_expanded {
                    let mut iter = lowered.into_iter();
                    let mut expanded = Vec::with_capacity(args.len());
                    for arg in args {
                        match arg {
                            syntax::CallArg::Value(_) => {
                                expanded.push(ArgPart::Normal(next_value(&mut iter)?));
                            }
                            syntax::CallArg::Expand(Some(_)) => {
                                expanded.push(ArgPart::Expand(next_value(&mut iter)?));
                            }
                            syntax::CallArg::Expand(None) => expanded.push(ArgPart::UnnamedExpand),
                            syntax::CallArg::Omitted => {
                                arg_lists.push(ArgList::OmittedCallerArgs);
                                return Ok(());
                            }
                        }
                    }
                    arg_lists.push(ArgList::Expanded(expanded));
                } else {
                    arg_lists.push(ArgList::Normal(lowered));
                }
            }
            ExprTask::AfterUnary(op) => {
                let src = pop_value(values)?;
                let dst = self.temp();
                self.emit_unary_expr(dst, op, src)?;
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterBinary(op) => {
                let rhs = pop_value(values)?;
                let lhs = pop_value(values)?;
                let dst = self.temp();
                if let Some(op) = compare_op(op) {
                    self.emit(MirInst::Compare { dst, op, lhs, rhs });
                } else if let Some(op) = binary_op(op) {
                    self.emit(MirInst::Binary { dst, op, lhs, rhs });
                } else {
                    return Err(TjsError::mir("unsupported binary operator"));
                }
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterInContextOf => {
                let this_obj = pop_value(values)?;
                let closure = pop_value(values)?;
                let dst = self.temp();
                self.emit(MirInst::ChangeThis {
                    dst,
                    closure,
                    this_obj,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterInstanceOf => {
                let class_name = pop_value(values)?;
                let value = pop_value(values)?;
                let dst = self.temp();
                self.emit(MirInst::IsInstanceOf {
                    dst,
                    value,
                    class_name,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterAssignment(op) => {
                let value = pop_value(values)?;
                let place = pop_place(places)?;
                let dst = self.temp();
                if op == syntax::AssignOp::Assign {
                    self.emit(MirInst::Assign {
                        place,
                        value,
                        result: Some(dst),
                    });
                } else if let Some(op) = assign_binary_op(op) {
                    self.emit(MirInst::AssignOp {
                        place,
                        op,
                        rhs: value,
                        result: Some(dst),
                    });
                } else {
                    return Err(TjsError::mir("unsupported assignment operator"));
                }
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterSwap => {
                let right = pop_place(places)?;
                let left = pop_place(places)?;
                self.emit(MirInst::Swap { left, right });
                values.push(lowerer.const_void());
            }
            ExprTask::AfterDelete => {
                let place = pop_place(places)?;
                let dst = self.temp();
                self.emit(MirInst::Delete {
                    dst: Some(dst),
                    place,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterPrefixUpdate(op) => {
                let place = pop_place(places)?;
                let dst = self.temp();
                self.emit(MirInst::Update {
                    place,
                    op: update_op_from_unary(op)?,
                    result: Some(dst),
                    result_value: UpdateResultValue::New,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterPostfixUpdate(op) => {
                let place = pop_place(places)?;
                let dst = self.temp();
                self.emit(MirInst::Update {
                    place,
                    op: update_op_from_unary(op)?,
                    result: Some(dst),
                    result_value: UpdateResultValue::Old,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterPostfixEval => {
                let source = pop_value(values)?;
                let dst = self.temp();
                self.emit(MirInst::Eval {
                    dst: Some(dst),
                    source,
                    mode: EvalMode::Expression,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterTypeOfValue => {
                let value = pop_value(values)?;
                let dst = self.temp();
                self.emit(MirInst::TypeOfValue { dst, value });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterTypeOfPlace => {
                let place = pop_place(places)?;
                let dst = self.temp();
                self.emit(MirInst::TypeOfPlace { dst, place });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterPropAccess => {
                let object = pop_value(values)?;
                values.push(self.read_place(Place::DefaultProperty {
                    object,
                    flags: FLAGS_DEFAULT_GET,
                }));
            }
            ExprTask::AfterShortCircuitLhs { op, rhs } => {
                self.push_short_circuit_tasks(lowerer, op, rhs, tasks, values)?;
            }
            ExprTask::EmitShortCircuitShortcut {
                block,
                result,
                shortcut,
                done_block,
            } => {
                self.start_block(block);
                self.emit(MirInst::Copy {
                    dst: result,
                    src: shortcut,
                });
                self.set_terminator(Terminator::Goto(done_block));
            }
            ExprTask::AfterShortCircuitRhs { result, done_block } => {
                let rhs = pop_value(values)?;
                self.emit(MirInst::ToBoolean {
                    dst: result,
                    src: rhs,
                });
                self.set_terminator(Terminator::Goto(done_block));
            }
            ExprTask::AfterIfOperatorCond { lhs } => {
                let cond = pop_value(values)?;
                let then_block = self.new_block(Some(lowerer.add_span(lhs.span)));
                let done_block = self.new_block(None);
                self.set_terminator(Terminator::Branch {
                    cond: Condition::Truthy(cond),
                    then_block,
                    else_block: done_block,
                });
                tasks.push(ExprTask::PushValue(lowerer.const_void()));
                tasks.push(ExprTask::StartBlock(done_block));
                tasks.push(ExprTask::GotoIfOpen(done_block));
                tasks.push(ExprTask::DiscardValue);
                tasks.push(ExprTask::Expr(lhs));
                tasks.push(ExprTask::StartBlock(then_block));
            }
            ExprTask::AfterConditionalCond {
                then_expr,
                else_expr,
                result,
            } => {
                let cond = pop_value(values)?;
                let then_block = self.new_block(Some(lowerer.add_span(then_expr.span)));
                let else_block = self.new_block(Some(lowerer.add_span(else_expr.span)));
                let done_block = self.new_block(None);
                self.set_terminator(Terminator::Branch {
                    cond: Condition::Truthy(cond),
                    then_block,
                    else_block,
                });
                tasks.push(ExprTask::PushValue(Value::Slot(result)));
                tasks.push(ExprTask::StartBlock(done_block));
                tasks.push(ExprTask::AfterConditionalBranch { result, done_block });
                tasks.push(ExprTask::Expr(else_expr));
                tasks.push(ExprTask::StartBlock(else_block));
                tasks.push(ExprTask::AfterConditionalBranch { result, done_block });
                tasks.push(ExprTask::Expr(then_expr));
                tasks.push(ExprTask::StartBlock(then_block));
            }
            ExprTask::AfterConditionalBranch { result, done_block } => {
                let value = pop_value(values)?;
                self.emit(MirInst::Copy {
                    dst: result,
                    src: value,
                });
                self.set_terminator(Terminator::Goto(done_block));
            }
            ExprTask::AfterCallArgs => {
                let args = pop_arg_list(arg_lists)?;
                let target = pop_call_target(call_targets)?;
                let dst = self.temp();
                self.emit(MirInst::Call {
                    dst: Some(dst),
                    target,
                    args,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::AfterNewCallee { args } => {
                let callee = pop_value(values)?;
                tasks.push(ExprTask::AfterNewArgs { callee });
                tasks.push(ExprTask::CallArgs(args));
            }
            ExprTask::AfterNewArgs { callee } => {
                let args = pop_arg_list(arg_lists)?;
                let dst = self.temp();
                self.emit(MirInst::New {
                    dst: Some(dst),
                    callee,
                    args,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::BuildArray {
                elements,
                value_count,
            } => {
                let lowered_values = split_values(values, value_count)?;
                let mut lowered_values = lowered_values.into_iter();
                let mut lowered = Vec::with_capacity(elements.len());
                for element in elements {
                    match element {
                        syntax::ArrayElement::Value(_) => {
                            lowered.push(ArrayElement::Value(next_value(&mut lowered_values)?));
                        }
                        syntax::ArrayElement::Hole => lowered.push(ArrayElement::Hole),
                    }
                }
                let dst = self.temp();
                self.emit(MirInst::BuildArray {
                    dst,
                    elements: lowered,
                });
                values.push(Value::Slot(dst));
            }
            ExprTask::BuildDictionary { plan, value_count } => {
                let lowered_values = split_values(values, value_count)?;
                let mut lowered_values = lowered_values.into_iter();
                let mut entries = Vec::with_capacity(plan.len());
                for key_plan in plan {
                    let key = match key_plan {
                        DictionaryKeyPlan::Direct(id) => DictionaryKey::Direct(id),
                        DictionaryKeyPlan::Computed => {
                            DictionaryKey::Computed(next_value(&mut lowered_values)?)
                        }
                    };
                    let value = next_value(&mut lowered_values)?;
                    entries.push(DictionaryEntry { key, value });
                }
                let dst = self.temp();
                self.emit(MirInst::BuildDictionary { dst, entries });
                values.push(Value::Slot(dst));
            }
            ExprTask::FinishComma { count } => {
                if count == 0 {
                    values.push(lowerer.const_void());
                } else {
                    let lowered = split_values(values, count)?;
                    values.push(*lowered.last().expect("non-empty comma expression"));
                }
            }
        }
        Ok(())
    }

    fn push_expr_task<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        expr: &'a syntax::Expr,
        tasks: &mut Vec<ExprTask<'a>>,
        values: &mut Vec<Value>,
        places: &mut Vec<Place>,
    ) -> Result<()> {
        match &expr.kind {
            syntax::ExprKind::Void => values.push(lowerer.const_void()),
            syntax::ExprKind::Null => {
                values.push(Value::Const(lowerer.add_const(MirConst::NullObject)));
            }
            syntax::ExprKind::Bool(value) => values.push(lowerer.const_bool(*value)),
            syntax::ExprKind::Integer(value) => {
                values.push(Value::Const(lowerer.add_const(MirConst::Integer(*value))));
            }
            syntax::ExprKind::Real(value) => {
                values.push(Value::Const(lowerer.add_const(MirConst::Real(*value))));
            }
            syntax::ExprKind::String(value) => {
                let id = lowerer.intern_string(value);
                values.push(Value::Const(lowerer.add_const(MirConst::String(id))));
            }
            syntax::ExprKind::Octet(value) => {
                values.push(Value::Const(
                    lowerer.add_const(MirConst::Octet(value.clone())),
                ));
            }
            syntax::ExprKind::RegExp { pattern, flags } => {
                let dst = self.temp();
                let pattern = lowerer.intern_string(pattern);
                let flags = lowerer.intern_string(flags);
                self.emit(MirInst::BuildRegExp {
                    dst,
                    pattern,
                    flags,
                });
                values.push(Value::Slot(dst));
            }
            syntax::ExprKind::Identifier(ident) => {
                values.push(self.read_ident(lowerer, ident, expr.span)?);
            }
            syntax::ExprKind::This => values.push(Value::Slot(SlotId::This)),
            syntax::ExprKind::Super => values.push(self.lower_super_expr(lowerer)?),
            syntax::ExprKind::Global => {
                values.push(self.global_value());
            }
            syntax::ExprKind::Nan => {
                values.push(Value::Const(lowerer.add_const(MirConst::Real(f64::NAN))));
            }
            syntax::ExprKind::Infinity => {
                values.push(Value::Const(
                    lowerer.add_const(MirConst::Real(f64::INFINITY)),
                ));
            }
            syntax::ExprKind::Array(elements) | syntax::ExprKind::ConstArray(elements) => {
                let value_count = elements
                    .iter()
                    .filter(|element| matches!(element, syntax::ArrayElement::Value(_)))
                    .count();
                tasks.push(ExprTask::BuildArray {
                    elements,
                    value_count,
                });
                for element in elements.iter().rev() {
                    if let syntax::ArrayElement::Value(expr) = element {
                        tasks.push(ExprTask::Expr(expr));
                    }
                }
            }
            syntax::ExprKind::Dictionary(entries) | syntax::ExprKind::ConstDictionary(entries) => {
                let mut plan = Vec::with_capacity(entries.len());
                let mut value_count = 0;
                for entry in entries {
                    match &entry.key.kind {
                        syntax::ExprKind::Identifier(name) => {
                            plan.push(DictionaryKeyPlan::Direct(lowerer.intern_string(&name.name)));
                        }
                        syntax::ExprKind::String(name) => {
                            plan.push(DictionaryKeyPlan::Direct(lowerer.intern_string(name)));
                        }
                        _ => {
                            plan.push(DictionaryKeyPlan::Computed);
                            value_count += 1;
                        }
                    }
                    value_count += 1;
                }
                tasks.push(ExprTask::BuildDictionary { plan, value_count });
                for entry in entries.iter().rev() {
                    tasks.push(ExprTask::Expr(&entry.value));
                    if !matches!(
                        &entry.key.kind,
                        syntax::ExprKind::Identifier(_) | syntax::ExprKind::String(_)
                    ) {
                        tasks.push(ExprTask::Expr(&entry.key));
                    }
                }
            }
            syntax::ExprKind::Unary { op, expr: inner } => match op {
                syntax::UnaryOp::IgnoreProp => {
                    if is_member_read_candidate(inner) {
                        tasks.push(ExprTask::ReadPlaceToValue);
                        self.push_member_read_place_task(
                            lowerer,
                            inner,
                            FLAGS_IGNORE_PROP_GET,
                            tasks,
                            places,
                        )?;
                    } else {
                        tasks.push(ExprTask::Expr(inner));
                    }
                }
                syntax::UnaryOp::PropAccess => {
                    tasks.push(ExprTask::AfterPropAccess);
                    tasks.push(ExprTask::Expr(inner));
                }
                syntax::UnaryOp::Delete => {
                    tasks.push(ExprTask::AfterDelete);
                    tasks.push(ExprTask::WritePlace(inner));
                }
                syntax::UnaryOp::Increment | syntax::UnaryOp::Decrement => {
                    tasks.push(ExprTask::AfterPrefixUpdate(*op));
                    tasks.push(ExprTask::WritePlace(inner));
                }
                syntax::UnaryOp::TypeOf if is_member_read_candidate(inner) => {
                    tasks.push(ExprTask::AfterTypeOfPlace);
                    self.push_member_read_place_task(
                        lowerer,
                        inner,
                        FLAGS_DEFAULT_GET,
                        tasks,
                        places,
                    )?;
                }
                syntax::UnaryOp::TypeOf => {
                    tasks.push(ExprTask::AfterTypeOfValue);
                    tasks.push(ExprTask::Expr(inner));
                }
                _ => {
                    tasks.push(ExprTask::AfterUnary(*op));
                    tasks.push(ExprTask::Expr(inner));
                }
            },
            syntax::ExprKind::Binary { op, lhs, rhs } => match op {
                syntax::BinaryOp::LogicalOr | syntax::BinaryOp::LogicalAnd => {
                    tasks.push(ExprTask::AfterShortCircuitLhs { op: *op, rhs });
                    tasks.push(ExprTask::Expr(lhs));
                }
                syntax::BinaryOp::If => {
                    tasks.push(ExprTask::AfterIfOperatorCond { lhs });
                    tasks.push(ExprTask::Expr(rhs));
                }
                syntax::BinaryOp::InContextOf => {
                    tasks.push(ExprTask::AfterInContextOf);
                    tasks.push(ExprTask::Expr(rhs));
                    tasks.push(ExprTask::Expr(lhs));
                }
                syntax::BinaryOp::InstanceOf => {
                    tasks.push(ExprTask::AfterInstanceOf);
                    tasks.push(ExprTask::Expr(rhs));
                    tasks.push(ExprTask::Expr(lhs));
                }
                _ => {
                    tasks.push(ExprTask::AfterBinary(*op));
                    tasks.push(ExprTask::Expr(rhs));
                    tasks.push(ExprTask::Expr(lhs));
                }
            },
            syntax::ExprKind::Assignment { op, target, value } => {
                if *op == syntax::AssignOp::Swap {
                    tasks.push(ExprTask::AfterSwap);
                    tasks.push(ExprTask::WritePlace(value));
                    tasks.push(ExprTask::WritePlace(target));
                } else {
                    tasks.push(ExprTask::AfterAssignment(*op));
                    tasks.push(ExprTask::Expr(value));
                    tasks.push(ExprTask::WritePlace(target));
                }
            }
            syntax::ExprKind::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                let result = self.temp();
                tasks.push(ExprTask::AfterConditionalCond {
                    then_expr,
                    else_expr,
                    result,
                });
                tasks.push(ExprTask::Expr(condition));
            }
            syntax::ExprKind::Member { object, property } => {
                tasks.push(ExprTask::ReadPlaceToValue);
                tasks.push(ExprTask::MemberPlaceAfterObject {
                    property,
                    flags: FLAGS_DEFAULT_GET,
                });
                tasks.push(ExprTask::Expr(object));
            }
            syntax::ExprKind::WithMember { property } => {
                places.push(Place::Member {
                    object: self.with_member_object(),
                    key: MemberKey::Direct(lowerer.intern_string(property)),
                    flags: FLAGS_DEFAULT_GET,
                });
                tasks.push(ExprTask::ReadPlaceToValue);
            }
            syntax::ExprKind::Index { object, index } => {
                tasks.push(ExprTask::ReadPlaceToValue);
                tasks.push(ExprTask::IndexPlaceAfterObject {
                    index,
                    flags: FLAGS_DEFAULT_GET,
                });
                tasks.push(ExprTask::Expr(object));
            }
            syntax::ExprKind::Call { callee, args } => {
                tasks.push(ExprTask::AfterCallArgs);
                tasks.push(ExprTask::CallArgs(args));
                tasks.push(ExprTask::CallTarget(callee));
            }
            syntax::ExprKind::New { callee, args } => {
                tasks.push(ExprTask::AfterNewCallee { args });
                tasks.push(ExprTask::Expr(callee));
            }
            syntax::ExprKind::Function(decl) => {
                let id = lowerer.lower_function_object(
                    decl,
                    ContextType::ExprFunction,
                    Some(self.object.id),
                )?;
                values.push(Value::Const(lowerer.add_const(MirConst::CodeObject(id))));
            }
            syntax::ExprKind::Postfix { op, expr: inner } => {
                if *op == syntax::UnaryOp::Eval {
                    tasks.push(ExprTask::AfterPostfixEval);
                    tasks.push(ExprTask::Expr(inner));
                } else {
                    tasks.push(ExprTask::AfterPostfixUpdate(*op));
                    tasks.push(ExprTask::WritePlace(inner));
                }
            }
            syntax::ExprKind::Comma(exprs) => {
                tasks.push(ExprTask::FinishComma { count: exprs.len() });
                for expr in exprs.iter().rev() {
                    tasks.push(ExprTask::Expr(expr));
                }
            }
        }
        Ok(())
    }

    fn push_write_place_task<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        expr: &'a syntax::Expr,
        tasks: &mut Vec<ExprTask<'a>>,
        places: &mut Vec<Place>,
    ) -> Result<()> {
        match &expr.kind {
            syntax::ExprKind::Unary {
                op: syntax::UnaryOp::IgnoreProp,
                expr,
            } => {
                tasks.push(ExprTask::ApplyIgnorePropToPlace);
                tasks.push(ExprTask::WritePlace(expr));
            }
            syntax::ExprKind::Unary {
                op: syntax::UnaryOp::PropAccess,
                expr,
            } => {
                tasks.push(ExprTask::DefaultPropertyPlaceAfterObject {
                    flags: FLAGS_DEFAULT_SET,
                });
                tasks.push(ExprTask::Expr(expr));
            }
            syntax::ExprKind::Identifier(ident) => {
                places.push(self.ident_write_place(lowerer, ident, expr.span)?);
            }
            syntax::ExprKind::WithMember { property } => {
                places.push(Place::Member {
                    object: self.with_member_object(),
                    key: MemberKey::Direct(lowerer.intern_string(property)),
                    flags: FLAGS_ENSURE_SET,
                });
            }
            syntax::ExprKind::Member { object, property } => {
                tasks.push(ExprTask::MemberPlaceAfterObject {
                    property,
                    flags: FLAGS_ENSURE_SET,
                });
                tasks.push(ExprTask::Expr(object));
            }
            syntax::ExprKind::Index { object, index } => {
                tasks.push(ExprTask::IndexPlaceAfterObject {
                    index,
                    flags: FLAGS_ENSURE_SET,
                });
                tasks.push(ExprTask::Expr(object));
            }
            _ => return Err(TjsError::mir("expression is not assignable")),
        }
        Ok(())
    }

    fn push_member_read_place_task<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        expr: &'a syntax::Expr,
        flags: DispatchFlags,
        tasks: &mut Vec<ExprTask<'a>>,
        places: &mut Vec<Place>,
    ) -> Result<()> {
        match &expr.kind {
            syntax::ExprKind::WithMember { property } => {
                places.push(Place::Member {
                    object: self.with_member_object(),
                    key: MemberKey::Direct(lowerer.intern_string(property)),
                    flags,
                });
            }
            syntax::ExprKind::Member { object, property } => {
                tasks.push(ExprTask::MemberPlaceAfterObject { property, flags });
                tasks.push(ExprTask::Expr(object));
            }
            syntax::ExprKind::Index { object, index } => {
                tasks.push(ExprTask::IndexPlaceAfterObject { index, flags });
                tasks.push(ExprTask::Expr(object));
            }
            _ => return Err(TjsError::mir("expression does not name a member place")),
        }
        Ok(())
    }

    fn push_call_target_task<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        callee: &'a syntax::Expr,
        tasks: &mut Vec<ExprTask<'a>>,
        call_targets: &mut Vec<CallTarget>,
    ) -> Result<()> {
        match &callee.kind {
            syntax::ExprKind::Member { object, property } => {
                tasks.push(ExprTask::CallTargetMemberAfterObject {
                    property,
                    flags: FLAGS_DEFAULT_GET,
                });
                tasks.push(ExprTask::Expr(object));
            }
            syntax::ExprKind::WithMember { property } => {
                call_targets.push(CallTarget::Member {
                    object: self.with_member_object(),
                    key: MemberKey::Direct(lowerer.intern_string(property)),
                    flags: FLAGS_DEFAULT_GET,
                });
            }
            syntax::ExprKind::Index { object, index } => {
                tasks.push(ExprTask::CallTargetIndexAfterObject {
                    index,
                    flags: FLAGS_DEFAULT_GET,
                });
                tasks.push(ExprTask::Expr(object));
            }
            syntax::ExprKind::Unary {
                op: syntax::UnaryOp::PropAccess,
                expr,
            } => {
                tasks.push(ExprTask::CallTargetDefaultPropertyAfterObject {
                    flags: FLAGS_DEFAULT_GET,
                });
                tasks.push(ExprTask::Expr(expr));
            }
            _ => {
                tasks.push(ExprTask::CallTargetValueAfter);
                tasks.push(ExprTask::Expr(callee));
            }
        }
        Ok(())
    }

    fn push_call_args_task<'a>(
        &mut self,
        args: &'a [syntax::CallArg],
        tasks: &mut Vec<ExprTask<'a>>,
        arg_lists: &mut Vec<ArgList>,
    ) {
        if args.len() == 1 && matches!(args[0], syntax::CallArg::Omitted) {
            arg_lists.push(ArgList::OmittedCallerArgs);
            return;
        }
        if args
            .iter()
            .any(|arg| matches!(arg, syntax::CallArg::Omitted))
        {
            arg_lists.push(ArgList::OmittedCallerArgs);
            return;
        }

        let mut value_count = 0;
        let mut saw_expanded = false;
        for arg in args {
            match arg {
                syntax::CallArg::Value(_) => value_count += 1,
                syntax::CallArg::Expand(Some(_)) => {
                    saw_expanded = true;
                    value_count += 1;
                }
                syntax::CallArg::Expand(None) => saw_expanded = true,
                syntax::CallArg::Omitted => {}
            }
        }
        tasks.push(ExprTask::BuildCallArgs {
            args,
            value_count,
            saw_expanded,
        });
        for arg in args.iter().rev() {
            match arg {
                syntax::CallArg::Value(expr) | syntax::CallArg::Expand(Some(expr)) => {
                    tasks.push(ExprTask::Expr(expr));
                }
                syntax::CallArg::Expand(None) | syntax::CallArg::Omitted => {}
            }
        }
    }

    fn push_short_circuit_tasks<'a>(
        &mut self,
        lowerer: &mut Lowerer,
        op: syntax::BinaryOp,
        rhs: &'a syntax::Expr,
        tasks: &mut Vec<ExprTask<'a>>,
        values: &mut Vec<Value>,
    ) -> Result<()> {
        let result = self.temp();
        let lhs = pop_value(values)?;
        let rhs_block = self.new_block(Some(lowerer.add_span(rhs.span)));
        let done_block = self.new_block(None);
        let shortcut_value = op == syntax::BinaryOp::LogicalOr;
        let shortcut = lowerer.const_bool(shortcut_value);
        let evaluate_when = if op == syntax::BinaryOp::LogicalOr {
            Condition::Falsey(lhs)
        } else {
            Condition::Truthy(lhs)
        };
        let shortcut_block = self.new_block(None);
        self.set_terminator(Terminator::Branch {
            cond: evaluate_when,
            then_block: rhs_block,
            else_block: shortcut_block,
        });

        tasks.push(ExprTask::PushValue(Value::Slot(result)));
        tasks.push(ExprTask::StartBlock(done_block));
        tasks.push(ExprTask::AfterShortCircuitRhs { result, done_block });
        tasks.push(ExprTask::Expr(rhs));
        tasks.push(ExprTask::StartBlock(rhs_block));
        tasks.push(ExprTask::EmitShortCircuitShortcut {
            block: shortcut_block,
            result,
            shortcut,
            done_block,
        });
        Ok(())
    }

    fn emit_unary_expr(&mut self, dst: SlotId, op: syntax::UnaryOp, src: Value) -> Result<()> {
        match op {
            syntax::UnaryOp::Plus => self.emit(MirInst::Convert {
                dst,
                op: ConvertOp::Number,
                src,
            }),
            syntax::UnaryOp::AsInt => self.emit(MirInst::Convert {
                dst,
                op: ConvertOp::Integer,
                src,
            }),
            syntax::UnaryOp::AsReal => self.emit(MirInst::Convert {
                dst,
                op: ConvertOp::Real,
                src,
            }),
            syntax::UnaryOp::AsString => self.emit(MirInst::Convert {
                dst,
                op: ConvertOp::String,
                src,
            }),
            syntax::UnaryOp::Minus => self.emit(MirInst::Unary {
                dst,
                op: UnaryOp::Negate,
                src,
            }),
            syntax::UnaryOp::LogicalNot => self.emit(MirInst::Unary {
                dst,
                op: UnaryOp::LogicalNot,
                src,
            }),
            syntax::UnaryOp::BitNot => self.emit(MirInst::Unary {
                dst,
                op: UnaryOp::BitNot,
                src,
            }),
            syntax::UnaryOp::TypeOf => self.emit(MirInst::TypeOfValue { dst, value: src }),
            syntax::UnaryOp::IsValid => self.emit(MirInst::CheckInvalidated { dst, target: src }),
            syntax::UnaryOp::Invalidate => self.emit(MirInst::Invalidate { dst, target: src }),
            syntax::UnaryOp::Sharp => self.emit(MirInst::Unary {
                dst,
                op: UnaryOp::Asc,
                src,
            }),
            syntax::UnaryOp::Dollar => self.emit(MirInst::Unary {
                dst,
                op: UnaryOp::Chr,
                src,
            }),
            syntax::UnaryOp::Eval => self.emit(MirInst::Eval {
                dst: Some(dst),
                source: src,
                mode: EvalMode::Expression,
            }),
            syntax::UnaryOp::IgnoreProp
            | syntax::UnaryOp::PropAccess
            | syntax::UnaryOp::Delete
            | syntax::UnaryOp::Increment
            | syntax::UnaryOp::Decrement => {
                return Err(TjsError::mir("unsupported unary operator"));
            }
        }
        Ok(())
    }

    fn read_ident(
        &mut self,
        lowerer: &mut Lowerer,
        ident: &syntax::Ident,
        _span: Span,
    ) -> Result<Value> {
        if let Some(slot) = self.resolve_local_ident_slot(lowerer, ident)? {
            return Ok(Value::Slot(slot));
        }

        let object = if self.force_global_context {
            self.global_value()
        } else {
            Value::Slot(SlotId::ThisProxy)
        };
        let place = Place::Member {
            object,
            key: MemberKey::Direct(lowerer.intern_string(&ident.name)),
            flags: FLAGS_DEFAULT_GET,
        };
        Ok(self.read_place(place))
    }

    fn ident_write_place(
        &mut self,
        lowerer: &mut Lowerer,
        ident: &syntax::Ident,
        _span: Span,
    ) -> Result<Place> {
        if let Some(slot) = self.resolve_local_ident_slot(lowerer, ident)? {
            return Ok(Place::Slot(slot));
        }

        let object = if self.force_global_context {
            self.global_value()
        } else {
            Value::Slot(SlotId::ThisProxy)
        };
        Ok(Place::Member {
            object,
            key: MemberKey::Direct(lowerer.intern_string(&ident.name)),
            flags: if lowerer.ident_is_property(ident) {
                FLAGS_DEFAULT_SET
            } else {
                FLAGS_IGNORE_PROP_SET
            },
        })
    }

    fn ident_declaration_place(
        &mut self,
        lowerer: &mut Lowerer,
        ident: &syntax::Ident,
        span: Span,
    ) -> Place {
        if ident.binding.is_some() && !lowerer.ident_is_global(ident) {
            if lowerer.ident_is_class_scoped(ident) {
                return Place::Member {
                    object: Value::Slot(SlotId::ThisProxy),
                    key: MemberKey::Direct(lowerer.intern_string(&ident.name)),
                    flags: FLAGS_IGNORE_PROP_SET,
                };
            }
            return Place::Slot(self.local(lowerer, ident.binding, Some(&ident.name), span));
        }

        Place::Member {
            object: Value::Slot(SlotId::ThisProxy),
            key: MemberKey::Direct(lowerer.intern_string(&ident.name)),
            flags: FLAGS_IGNORE_PROP_SET,
        }
    }

    fn resolve_local_ident_slot(
        &self,
        lowerer: &Lowerer,
        ident: &syntax::Ident,
    ) -> Result<Option<SlotId>> {
        let Some(binding) = ident.binding else {
            return Ok(None);
        };
        if lowerer.ident_is_global(ident) {
            return Ok(None);
        }
        if lowerer.ident_is_class_scoped(ident) {
            return Ok(None);
        }
        self.binding_slots.get(&binding).copied().map_or_else(
            || {
                Err(TjsError::mir(format!(
                    "captured binding `{}` is not supported by MIR yet",
                    ident.name
                )))
            },
            |slot| Ok(Some(slot)),
        )
    }

    fn with_member_object(&mut self) -> Value {
        if let Some(object) = self.with_stack.last().copied() {
            object
        } else {
            let dst = self.temp();
            self.emit(MirInst::LoadGlobal { dst });
            Value::Slot(dst)
        }
    }
}

fn push_stmt_tasks<'a>(tasks: &mut Vec<StmtTask<'a>>, statements: &'a [syntax::Stmt]) {
    for statement in statements.iter().rev() {
        tasks.push(StmtTask::Stmt(statement));
    }
}

fn pop_value(values: &mut Vec<Value>) -> Result<Value> {
    values
        .pop()
        .ok_or_else(|| TjsError::mir("expression value stack underflow"))
}

fn pop_place(places: &mut Vec<Place>) -> Result<Place> {
    places
        .pop()
        .ok_or_else(|| TjsError::mir("expression place stack underflow"))
}

fn pop_call_target(call_targets: &mut Vec<CallTarget>) -> Result<CallTarget> {
    call_targets
        .pop()
        .ok_or_else(|| TjsError::mir("call target stack underflow"))
}

fn pop_arg_list(arg_lists: &mut Vec<ArgList>) -> Result<ArgList> {
    arg_lists
        .pop()
        .ok_or_else(|| TjsError::mir("call argument stack underflow"))
}

fn split_values(values: &mut Vec<Value>, count: usize) -> Result<Vec<Value>> {
    if values.len() < count {
        return Err(TjsError::mir("expression value stack underflow"));
    }
    Ok(values.split_off(values.len() - count))
}

fn next_value(iter: &mut impl Iterator<Item = Value>) -> Result<Value> {
    iter.next()
        .ok_or_else(|| TjsError::mir("expression value stack underflow"))
}

fn is_member_read_candidate(expr: &syntax::Expr) -> bool {
    matches!(
        expr.kind,
        syntax::ExprKind::WithMember { .. }
            | syntax::ExprKind::Member { .. }
            | syntax::ExprKind::Index { .. }
    )
}

fn update_op_from_unary(op: syntax::UnaryOp) -> Result<UpdateOp> {
    match op {
        syntax::UnaryOp::Increment => Ok(UpdateOp::Inc),
        syntax::UnaryOp::Decrement => Ok(UpdateOp::Dec),
        _ => Err(TjsError::mir("unsupported update operator")),
    }
}

fn compare_op(op: syntax::BinaryOp) -> Option<CompareOp> {
    Some(match op {
        syntax::BinaryOp::Equal => CompareOp::Equal,
        syntax::BinaryOp::NotEqual => CompareOp::NotEqual,
        syntax::BinaryOp::DiscernEqual => CompareOp::DiscernEqual,
        syntax::BinaryOp::DiscernNotEqual => CompareOp::DiscernNotEqual,
        syntax::BinaryOp::Less => CompareOp::LessThan,
        syntax::BinaryOp::Greater => CompareOp::GreaterThan,
        syntax::BinaryOp::LessEqual => CompareOp::LessEqual,
        syntax::BinaryOp::GreaterEqual => CompareOp::GreaterEqual,
        _ => return None,
    })
}

fn binary_op(op: syntax::BinaryOp) -> Option<BinaryOp> {
    Some(match op {
        syntax::BinaryOp::BitOr => BinaryOp::BitOr,
        syntax::BinaryOp::BitXor => BinaryOp::BitXor,
        syntax::BinaryOp::BitAnd => BinaryOp::BitAnd,
        syntax::BinaryOp::ShiftArithmeticRight => BinaryOp::ShiftArithmeticRight,
        syntax::BinaryOp::ShiftLeft => BinaryOp::ShiftLeft,
        syntax::BinaryOp::ShiftLogicalRight => BinaryOp::ShiftLogicalRight,
        syntax::BinaryOp::Add => BinaryOp::Add,
        syntax::BinaryOp::Sub => BinaryOp::Sub,
        syntax::BinaryOp::Mod => BinaryOp::Mod,
        syntax::BinaryOp::Div => BinaryOp::Div,
        syntax::BinaryOp::Idiv => BinaryOp::Idiv,
        syntax::BinaryOp::Mul => BinaryOp::Mul,
        syntax::BinaryOp::LogicalOr => BinaryOp::LogicalOr,
        syntax::BinaryOp::LogicalAnd => BinaryOp::LogicalAnd,
        _ => return None,
    })
}

fn assign_binary_op(op: syntax::AssignOp) -> Option<BinaryOp> {
    Some(match op {
        syntax::AssignOp::BitAnd => BinaryOp::BitAnd,
        syntax::AssignOp::BitOr => BinaryOp::BitOr,
        syntax::AssignOp::BitXor => BinaryOp::BitXor,
        syntax::AssignOp::Sub => BinaryOp::Sub,
        syntax::AssignOp::Add => BinaryOp::Add,
        syntax::AssignOp::Mod => BinaryOp::Mod,
        syntax::AssignOp::Div => BinaryOp::Div,
        syntax::AssignOp::Idiv => BinaryOp::Idiv,
        syntax::AssignOp::Mul => BinaryOp::Mul,
        syntax::AssignOp::LogicalOr => BinaryOp::LogicalOr,
        syntax::AssignOp::LogicalAnd => BinaryOp::LogicalAnd,
        syntax::AssignOp::ShiftLogicalRight => BinaryOp::ShiftLogicalRight,
        syntax::AssignOp::ShiftLeft => BinaryOp::ShiftLeft,
        syntax::AssignOp::ShiftArithmeticRight => BinaryOp::ShiftArithmeticRight,
        syntax::AssignOp::Assign | syntax::AssignOp::Swap => return None,
    })
}

fn optimize_module(module: &mut MirModule) {
    for index in 0..module.objects.len() {
        fold_constant_branches(module, index);
        remove_nops(&mut module.objects[index]);
        collapse_empty_gotos(&mut module.objects[index]);
        remove_unreachable_blocks(&mut module.objects[index]);
    }
}

fn remove_nops(object: &mut MirObject) {
    for block in &mut object.blocks {
        block.insts.retain(|inst| !matches!(inst, MirInst::Nop));
    }
}

fn fold_constant_branches(module: &mut MirModule, object_index: usize) {
    let constants = module.constants.clone();
    for block in &mut module.objects[object_index].blocks {
        let Terminator::Branch {
            cond,
            then_block,
            else_block,
        } = &block.terminator
        else {
            continue;
        };
        let folded = match cond {
            Condition::Truthy(value) => const_truthy(&constants, *value),
            Condition::Falsey(value) => const_truthy(&constants, *value).map(|value| !value),
            Condition::ArgNeedsDefault(_) | Condition::Compare { .. } => None,
        };
        if let Some(value) = folded {
            block.terminator = Terminator::Goto(if value { *then_block } else { *else_block });
        }
    }
}

fn const_truthy(constants: &[MirConst], value: Value) -> Option<bool> {
    let Value::Const(id) = value else {
        return None;
    };
    match constants.get(id.0 as usize)? {
        MirConst::Void | MirConst::NullObject => Some(false),
        MirConst::Integer(value) => Some(*value != 0),
        MirConst::Real(value) if *value == 0.0 || value.is_nan() => Some(false),
        MirConst::Real(_) => Some(true),
        _ => None,
    }
}

fn collapse_empty_gotos(object: &mut MirObject) {
    let mut redirects = BTreeMap::new();
    for block in &object.blocks {
        if block.id != object.entry
            && block.insts.is_empty()
            && let Terminator::Goto(target) = block.terminator
            && target != block.id
        {
            redirects.insert(block.id, target);
        }
    }
    if redirects.is_empty() {
        return;
    }
    let resolve = |mut target: BlockId| {
        let mut seen = BTreeSet::new();
        while let Some(next) = redirects.get(&target).copied() {
            if !seen.insert(target) {
                break;
            }
            target = next;
        }
        target
    };
    for block in &mut object.blocks {
        rewrite_terminator_targets(&mut block.terminator, &redirects);
    }
    for region in &mut object.exception_regions {
        region.entry = resolve(region.entry);
        region.catch = resolve(region.catch);
        for block in &mut region.protected_blocks {
            *block = resolve(*block);
        }
        region.protected_blocks.sort();
        region.protected_blocks.dedup();
    }
}

fn rewrite_terminator_targets(term: &mut Terminator, redirects: &BTreeMap<BlockId, BlockId>) {
    let resolve = |mut target: BlockId| {
        let mut seen = BTreeSet::new();
        while let Some(next) = redirects.get(&target).copied() {
            if !seen.insert(target) {
                break;
            }
            target = next;
        }
        target
    };
    match term {
        Terminator::Goto(target) => *target = resolve(*target),
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => {
            *then_block = resolve(*then_block);
            *else_block = resolve(*else_block);
        }
        Terminator::LeaveTry { next, .. } => *next = resolve(*next),
        Terminator::Return { .. } | Terminator::Throw { .. } | Terminator::Unreachable => {}
    }
}

fn remove_unreachable_blocks(object: &mut MirObject) {
    let mut reachable = BTreeSet::new();
    let mut stack = vec![object.entry];
    while let Some(id) = stack.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let Some(block) = object.blocks.iter().find(|block| block.id == id) else {
            continue;
        };
        for next in terminator_successors(&block.terminator) {
            stack.push(next);
        }
        for region in &object.exception_regions {
            if region.protected_blocks.contains(&id) {
                stack.push(region.catch);
            }
        }
    }
    object.blocks.retain(|block| reachable.contains(&block.id));
    let parent_by_region = object
        .exception_regions
        .iter()
        .map(|region| (region.id, region.parent))
        .collect::<BTreeMap<_, _>>();
    for region in &mut object.exception_regions {
        region
            .protected_blocks
            .retain(|block| reachable.contains(block));
        region.protected_blocks.sort();
        region.protected_blocks.dedup();
    }
    let leave_regions = object
        .blocks
        .iter()
        .filter_map(|block| match block.terminator {
            Terminator::LeaveTry { region, .. } => Some(region),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let retained_regions = object
        .exception_regions
        .iter()
        .filter(|region| {
            reachable.contains(&region.entry)
                && reachable.contains(&region.catch)
                && (!region.protected_blocks.is_empty() || leave_regions.contains(&region.id))
        })
        .map(|region| region.id)
        .collect::<BTreeSet<_>>();
    object
        .exception_regions
        .retain(|region| retained_regions.contains(&region.id));
    for region in &mut object.exception_regions {
        let mut parent = region.parent;
        while let Some(parent_id) = parent {
            if retained_regions.contains(&parent_id) {
                break;
            }
            parent = parent_by_region.get(&parent_id).copied().flatten();
        }
        region.parent = parent;
    }
}

fn terminator_successors(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Goto(id) => vec![*id],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        Terminator::LeaveTry { next, .. } => vec![*next],
        Terminator::Return { .. } | Terminator::Throw { .. } | Terminator::Unreachable => {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::error::TjsErrorKind;
    use crate::{FrontendOptions, analyze_script};

    use super::*;

    fn lower_result(source: &str) -> Result<MirModule> {
        let output = analyze_script("inline.tjs", source, FrontendOptions::default());
        let program = output.value.unwrap_or_else(|| {
            panic!("frontend failed: {:?}", output.diagnostics);
        });
        lower_hir_program(&program, "inline.tjs", source)
    }

    fn lower(source: &str) -> MirModule {
        lower_result(source).expect("lower")
    }

    #[test]
    fn lowers_frontend_control_flow_to_blocks() {
        let module = lower("var x = 0; while (x < 3) { x += 1; } return x;");
        module.validate().expect("valid MIR");
        let top = &module.objects[module.top_level.0 as usize];
        assert!(top.blocks.len() >= 4);
        assert!(module.snapshot().contains("AssignOp"));
    }

    #[test]
    fn lowers_try_catch_to_exception_region() {
        let module = lower(r#"try { var x = 1; } catch (e) { return e; }"#);
        let top = &module.objects[module.top_level.0 as usize];
        assert_eq!(top.exception_regions.len(), 1);
        assert!(module.snapshot().contains("LeaveTry"));
    }

    #[test]
    fn try_region_starts_after_prior_instructions() {
        let module = lower("foo(); try { bar(); } catch (e) { return e; }");
        let top = &module.objects[module.top_level.0 as usize];
        let region = top.exception_regions.first().expect("exception region");
        assert_ne!(region.entry, top.entry);
        assert!(!region.protected_blocks.contains(&top.entry));
        let entry = top
            .blocks
            .iter()
            .find(|block| block.id == top.entry)
            .expect("entry block");
        assert!(
            entry
                .insts
                .iter()
                .any(|inst| matches!(inst, MirInst::Call { .. }))
        );
    }

    #[test]
    fn nested_function_captures_are_rejected_until_modeled() {
        let err = lower_result("function f() { var x = 1; function g() { return x; } }")
            .expect_err("capture should be rejected");
        assert_eq!(err.kind, TjsErrorKind::Mir);
        assert!(err.message.contains("captured binding `x`"));
    }

    #[test]
    fn default_arg_expressions_are_guarded() {
        let module = lower("function f(a = side_effect()) { return a; }");
        let function = module
            .objects
            .iter()
            .find(|object| object.context == ContextType::Function)
            .expect("function object");
        let entry = function
            .blocks
            .iter()
            .find(|block| block.id == function.entry)
            .expect("entry block");
        let Terminator::Branch {
            cond: Condition::ArgNeedsDefault(0),
            then_block,
            ..
        } = entry.terminator
        else {
            panic!("default arg entry should branch on missing arg");
        };
        assert!(
            !entry
                .insts
                .iter()
                .any(|inst| matches!(inst, MirInst::Call { .. }))
        );
        let default_block = function
            .blocks
            .iter()
            .find(|block| block.id == then_block)
            .expect("default block");
        assert!(
            default_block
                .insts
                .iter()
                .any(|inst| matches!(inst, MirInst::Call { .. }))
        );
    }

    #[test]
    fn nested_try_catch_stays_protected_by_outer_try() {
        let module = lower(
            r#"
            try {
                try { throw 1; } catch (e) { throw 2; }
            } catch (e) {
                return e;
            }
            "#,
        );
        let top = &module.objects[module.top_level.0 as usize];
        assert_eq!(top.exception_regions.len(), 2);
        let outer = top
            .exception_regions
            .iter()
            .find(|region| region.parent.is_none())
            .expect("outer region");
        let inner = top
            .exception_regions
            .iter()
            .find(|region| region.parent == Some(outer.id))
            .expect("inner region");
        assert!(outer.protected_blocks.contains(&inner.catch));
    }

    #[test]
    fn in_region_breaks_do_not_leave_surrounding_try() {
        let module = lower("try { while (flag) { break; } throw 1; } catch (e) { return e; }");
        let top = &module.objects[module.top_level.0 as usize];
        assert_eq!(top.exception_regions.len(), 1);
        assert!(
            top.blocks
                .iter()
                .all(|block| !matches!(block.terminator, Terminator::LeaveTry { .. }))
        );
    }

    #[test]
    fn unreachable_try_regions_are_pruned() {
        let module = lower("if (false) { try { var x = 1; } catch (e) { return e; } } return 3;");
        let top = &module.objects[module.top_level.0 as usize];
        assert!(top.exception_regions.is_empty());
    }

    #[test]
    fn unresolved_identifiers_lower_to_this_proxy_members() {
        let module = lower("missing = 1; return missing;");
        let snapshot = module.snapshot();
        assert!(snapshot.contains("ThisProxy"));
        assert!(snapshot.contains("Assign"));
    }

    #[test]
    fn lowers_functions_classes_properties_and_calls() {
        let module = lower(
            r#"
            function f(a, b = 1, rest*) { return a + b; }
            property p { getter { return 1; } setter(v) { value = v; } }
            class C extends Base { function m() { return f(1, *args, *); } }
            "#,
        );
        module.validate().expect("valid MIR");
        let snapshot = module.snapshot();
        assert!(snapshot.contains("Function"));
        assert!(snapshot.contains("PropertyGetter"));
        assert!(snapshot.contains("Class"));
        assert!(snapshot.contains("ApplyClassExtender"));
        assert!(snapshot.contains("Expanded"));
    }

    #[test]
    fn lowers_deep_statement_nesting_iteratively() {
        let span = Span::empty(0);
        let mut statement = syntax::Stmt::new(syntax::StmtKind::Empty, span);
        for _ in 0..2048 {
            statement = syntax::Stmt::new(syntax::StmtKind::Block(vec![statement]), span);
        }
        let program = hir::Program {
            statements: vec![statement],
            span,
            scopes: Vec::new(),
            bindings: Vec::new(),
        };
        let module = lower_hir_program(&program, "deep.tjs", "").expect("lower");
        module.validate().expect("valid MIR");
    }

    #[test]
    fn lowers_deep_expression_nesting_iteratively() {
        let span = Span::empty(0);
        let mut expr = syntax::Expr::new(syntax::ExprKind::Integer(0), span);
        for _ in 0..2048 {
            expr = syntax::Expr::new(
                syntax::ExprKind::Binary {
                    op: syntax::BinaryOp::Add,
                    lhs: Box::new(expr),
                    rhs: Box::new(syntax::Expr::new(syntax::ExprKind::Integer(1), span)),
                },
                span,
            );
        }
        let program = hir::Program {
            statements: vec![syntax::Stmt::new(syntax::StmtKind::Expr(expr), span)],
            span,
            scopes: Vec::new(),
            bindings: Vec::new(),
        };
        let module = lower_hir_program(&program, "deep_expr.tjs", "").expect("lower");
        module.validate().expect("valid MIR");
    }

    #[test]
    fn source_span_mapper_uses_precomputed_utf16_offsets() {
        let mapper = SourceSpanMapper::new("a\u{e9}\u{10400}b");
        let span = mapper.to_source_span(Span::new(1, 7));
        assert_eq!(span.byte_start, 1);
        assert_eq!(span.byte_end, 7);
        assert_eq!(span.utf16_start, 1);
        assert_eq!(span.utf16_end, 4);

        let ascii = SourceSpanMapper::new("abc");
        let span = ascii.to_source_span(Span::new(1, 2));
        assert_eq!(span.utf16_start, 1);
        assert_eq!(span.utf16_end, 2);
    }

    #[test]
    fn if_operator_discards_then_value_in_larger_expression() {
        let module = lower("return 2 + (side_effect() if false);");
        module.validate().expect("valid MIR");
    }
}

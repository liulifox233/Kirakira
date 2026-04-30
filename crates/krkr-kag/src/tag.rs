use crate::source::{SourceLocation, SourceSpan};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    pub tagname: String,
    pub attributes: Vec<Attribute>,
    pub origin: TagOrigin,
    pub span: SourceSpan,
    pub location: SourceLocation,
}

impl Tag {
    pub fn new(
        tagname: impl Into<String>,
        attributes: Vec<Attribute>,
        origin: TagOrigin,
        span: SourceSpan,
        location: SourceLocation,
    ) -> Self {
        Self {
            tagname: tagname.into(),
            attributes,
            origin,
            span,
            location,
        }
    }

    pub fn character(text: impl Into<String>, span: SourceSpan, location: SourceLocation) -> Self {
        Self::new(
            "ch",
            vec![Attribute::named(
                "text",
                AttributeValue::Literal(text.into()),
            )],
            TagOrigin::Character,
            span,
            location,
        )
    }

    pub fn newline(span: SourceSpan, location: SourceLocation) -> Self {
        Self::new(
            "r",
            vec![Attribute::named(
                "eol",
                AttributeValue::Literal("true".into()),
            )],
            TagOrigin::Newline,
            span,
            location,
        )
    }

    pub fn interrupt() -> Self {
        Self::new(
            "interrupt",
            Vec::new(),
            TagOrigin::Interrupt,
            SourceSpan::empty(0),
            SourceLocation::default(),
        )
    }

    pub fn attr(&self, name: &str) -> Option<&AttributeValue> {
        self.attributes
            .iter()
            .find_map(|attribute| match attribute {
                Attribute::Named {
                    name: attr_name,
                    value,
                } if attr_name == name => Some(value),
                _ => None,
            })
    }

    pub fn take_attr(&mut self, name: &str) -> Option<AttributeValue> {
        let index = self
            .attributes
            .iter()
            .position(|attribute| matches!(attribute, Attribute::Named { name: attr_name, .. } if attr_name == name))?;
        match self.attributes.remove(index) {
            Attribute::Named { value, .. } => Some(value),
            Attribute::Spread => None,
        }
    }

    pub fn literal_attr(&self, name: &str) -> Option<&str> {
        self.attr(name).and_then(AttributeValue::as_literal)
    }

    pub fn set_origin_recursive(&mut self, origin: TagOrigin) {
        self.origin = origin;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Attribute {
    Named { name: String, value: AttributeValue },
    Spread,
}

impl Attribute {
    pub fn named(name: impl Into<String>, value: AttributeValue) -> Self {
        Self::Named {
            name: name.into(),
            value,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Named { name, .. } => Some(name),
            Self::Spread => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttributeValue {
    Literal(String),
    Expression(String),
    MacroArgument(String),
}

impl AttributeValue {
    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    pub fn raw(&self) -> &str {
        match self {
            Self::Literal(value) | Self::Expression(value) | Self::MacroArgument(value) => value,
        }
    }

    pub fn as_literal(&self) -> Option<&str> {
        match self {
            Self::Literal(value) => Some(value),
            Self::Expression(_) | Self::MacroArgument(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TagOrigin {
    Bracket,
    CommandLine,
    Character,
    Newline,
    Interrupt,
    MacroExpansion { name: String },
}

use std::cmp::Ordering;
use std::collections::HashSet;
use std::mem;

use cssparser::{
    parse_important, AtRuleParser, CowRcStr, DeclarationListParser, DeclarationParser, Delimiter,
    ParseError, Parser, SourceLocation,
};
use smallbitvec::SmallBitVec;

use crate::style::properties::id::{LonghandId, PropertyId};
use crate::style::select::Specificity;
use crate::style::values::specified::length::LengthPercentage;
use crate::style::values::specified::FontSize;
use crate::style::CascadeOrigin;
use crate::style::{CssOrigin, StyleParseErrorKind};
use std::borrow::Borrow;

pub mod id;
pub mod longhands;

/// Parses raw parser input into a block of property declarations.
pub fn parse_property_declaration_list(input: &mut Parser) -> PropertyDeclarationBlock {
    let mut block = PropertyDeclarationBlock::new();
    let prop_parser = PropertyDeclarationParser {
        declarations: Vec::new(),
    };
    let mut decl_iter = DeclarationListParser::new(input, prop_parser);
    while let Some(declaration) = decl_iter.next() {
        match declaration {
            Ok(importance) => {
                let decls: Vec<PropertyDeclaration> =
                    decl_iter.parser.declarations.drain(..).collect();
                for decl in decls.iter() {
                    block.add_declaration(decl.clone(), importance);
                }
            }
            Err(parse_err) => {
                dbg!(parse_err);
            }
        }
    }
    block
}

/// A struct to parse property declarations.
pub struct PropertyDeclarationParser {
    declarations: Vec<PropertyDeclaration>,
    //    /// The last parsed property id (if any).
    //    last_parsed_property_id: Option<PropertyId>,
}

impl<'i> DeclarationParser<'i> for PropertyDeclarationParser {
    type Declaration = Importance;
    type Error = StyleParseErrorKind<'i>;

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Importance, ParseError<'i, Self::Error>> {
        // Try to match (parse) the specified declaration `name` into a known property ID.
        let id = match PropertyId::parse(&name) {
            Some(id) => id,
            None => {
                return Err(input.new_custom_error(StyleParseErrorKind::UnknownProperty(name)));
            }
        };
        input.parse_until_before(Delimiter::Bang, |input| {
            PropertyDeclaration::parse_into(&mut self.declarations, id, input)
        })?;
        let importance = match input.try_parse(parse_important) {
            Ok(()) => Importance::Important,
            Err(_) => Importance::Normal,
        };
        // In case there is still unparsed text in the declaration, we should roll back.
        input.expect_exhausted()?;
        Ok(importance)
    }
}

/// Kosmonaut currently doesn't support @rules.  Fallback to the default "error" implementation.
/// TODO: Support atrules
impl<'i> AtRuleParser<'i> for PropertyDeclarationParser {
    type PreludeNoBlock = ();
    type PreludeBlock = ();
    type AtRule = Importance;
    type Error = StyleParseErrorKind<'i>;
}

#[derive(Clone, Debug, Default)]
pub struct PropertyDeclarationBlock {
    /// The group of declarations, along with their importance.
    declarations: Vec<PropertyDeclaration>,

    /// The "important" flag for each declaration in `declarations`.
    declarations_importance: SmallBitVec,

    longhands: HashSet<LonghandId>,
}

impl PropertyDeclarationBlock {
    /// Adds a new declaration to the block, de-duping with any existing property declarations
    /// of the same type.
    pub fn add_declaration(
        &mut self,
        mut new_decl: PropertyDeclaration,
        new_importance: Importance,
    ) {
        let mut swap_index = None;
        for (i, existing_decl) in self.declarations.iter().enumerate() {
            if mem::discriminant(existing_decl) == mem::discriminant(&new_decl) {
                // the props are the same "type", e.g. both `font-size, both `display`, etc
                // take the `new_decl`, since the latest/newest prop should always be taken
                swap_index = Some(i);
            }
        }

        if let Some(idx) = swap_index {
            mem::swap(&mut self.declarations[idx], &mut new_decl);
            self.declarations_importance
                .set(idx, new_importance.important());
        } else {
            self.declarations.push(new_decl);
            self.declarations_importance
                .push(new_importance.important());
        }
    }
}

impl PropertyDeclarationBlock {
    pub fn new() -> PropertyDeclarationBlock {
        Self::default()
    }

    pub fn declarations(&self) -> &[PropertyDeclaration] {
        &self.declarations
    }

    pub fn remove_decl(&mut self, index: usize) {
        &self.declarations.remove(index);
    }

    pub fn declarations_importance(&self) -> &SmallBitVec {
        &self.declarations_importance
    }
}

impl PropertyDeclaration {
    pub fn parse_into<'i, 't>(
        declarations: &mut Vec<PropertyDeclaration>,
        id: PropertyId,
        input: &mut Parser<'i, 't>,
    ) -> Result<(), ParseError<'i, StyleParseErrorKind<'i>>> {
        match id {
            PropertyId::Longhand(long_id) => match long_id {
                LonghandId::Display => {}
                LonghandId::FontSize => {
                    declarations.push(PropertyDeclaration::FontSize(FontSize::parse(input)?));
                }
                LonghandId::MarginLeft => {
                    // TODO: This should be LengthPercentageOrAuto, but we currently don't handle the `auto` keyword - https://www.w3.org/TR/css-box-3/#property-index
                    declarations.push(PropertyDeclaration::MarginLeft(LengthPercentage::parse(
                        input,
                    )?))
                }
                _ => {}
            },
            PropertyId::Shorthand(_short_id) => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
#[repr(u16)]
pub enum PropertyDeclaration {
    // Property(value)
    Display(crate::style::values::specified::Display),
    FontSize(crate::style::values::specified::FontSize),
    // TODO: This should be LengthPercentageOrAuto, but we currently don't handle the `auto` keyword - https://www.w3.org/TR/css-box-3/#property-index
    MarginLeft(crate::style::values::specified::length::LengthPercentage),
}

/// A property declaration with contextual information, such as its importance, specificity,
/// origin, and source location, all of which likely deriving from its parent style rule.
#[derive(Clone, Debug)]
pub struct ContextualPropertyDeclaration {
    pub inner_decl: PropertyDeclaration,
    pub important: bool,
    pub origin: CssOrigin,
    pub source_location: Option<SourceLocation>,
    pub specificity: Specificity,
}

/// Wrapper over a Vec<PropertyDeclaration> to provide efficient helpers over common operations
/// such as determining the existence of a type of property declaration.
#[derive(Clone, Debug)]
pub struct ContextualPropertyDeclarations {
    /// The actual context property declarations.
    decls: Vec<ContextualPropertyDeclaration>,
    /// The LonghandIds present in this container.
    longhands: HashSet<LonghandId>,
}

impl ContextualPropertyDeclarations {
    #[inline]
    pub fn new() -> Self {
        ContextualPropertyDeclarations {
            decls: Vec::new(),
            longhands: HashSet::new(),
        }
    }

    #[inline]
    pub fn sort(&mut self) {
        self.decls.as_mut_slice().sort();
    }

    #[inline]
    pub fn contains(&self, longhand: LonghandId) -> bool {
        self.longhands.contains(&longhand)
    }

    #[inline]
    pub fn add(&mut self, new_decl: ContextualPropertyDeclaration) {
        self.longhands
            .insert(LonghandId::from(&new_decl.inner_decl).clone());
        self.decls.push(new_decl);
    }
}

/// Much of Kosmonaut's cascade algorithm is in this implementation — namely, the first two top-level
/// bullet points.  The final deciding factor in the cascade, order of appearance, can't possibly
/// be exercised here.
///
/// https://www.w3.org/TR/2018/CR-css-cascade-3-20180828/#cascade-origin
/// The cascade sorts declarations according to the following criteria, in descending order of priority:
///
/// * Origin and Importance
///   The origin of a declaration is based on where it comes from and its importance is whether or
///   not it is declared !important (see below).  The precedence of the various origins is, in descending order:
///     1. Transition declarations [css-transitions-1]
///     2. Important user agent declarations
///     3. Important user declarations
///     4. Important author declarations
///     5. Animation declarations [css-animations-1]
///     6. Normal author declarations
///     7. Normal user declarations
///     8. Normal user agent declarations
///
///     Declarations from origins earlier in this list win over declarations from later origins.
/// * Specificity
///     The Selectors module [SELECT] describes how to compute the specificity of a selector. Each declaration
///     has the same specificity as the style rule it appears in. For the purpose of this step, declarations
///         * Declarations from style attributes are ordered according to the document order of the element the style attribute appears on, and are all placed after any style sheets.
impl Ord for ContextualPropertyDeclaration {
    fn cmp(&self, other: &Self) -> Ordering {
        fn cmp_important_origins(a: &CssOrigin, b: &CssOrigin) -> Ordering {
            match (a, b) {
                (CssOrigin::Inline, CssOrigin::Inline)
                | (CssOrigin::Inline, CssOrigin::Embedded)
                | (CssOrigin::Embedded, CssOrigin::Inline)
                | (CssOrigin::Embedded, CssOrigin::Embedded) => Ordering::Equal,
                (CssOrigin::Inline, CssOrigin::Sheet(other_sheet_origin))
                | (CssOrigin::Embedded, CssOrigin::Sheet(other_sheet_origin)) => {
                    match &other_sheet_origin.cascade_origin {
                        CascadeOrigin::UserAgent | CascadeOrigin::User => Ordering::Less,
                        CascadeOrigin::Author => Ordering::Equal,
                    }
                }
                (CssOrigin::Sheet(self_sheet_origin), CssOrigin::Inline)
                | (CssOrigin::Sheet(self_sheet_origin), CssOrigin::Embedded) => {
                    match &self_sheet_origin.cascade_origin {
                        CascadeOrigin::UserAgent | CascadeOrigin::User => Ordering::Greater,
                        CascadeOrigin::Author => Ordering::Equal,
                    }
                }
                (CssOrigin::Sheet(self_sheet_origin), CssOrigin::Sheet(other_sheet_origin)) => {
                    match (
                        &self_sheet_origin.cascade_origin,
                        &other_sheet_origin.cascade_origin,
                    ) {
                        (CascadeOrigin::UserAgent, CascadeOrigin::UserAgent) => Ordering::Equal,
                        (CascadeOrigin::UserAgent, CascadeOrigin::User)
                        | (CascadeOrigin::UserAgent, CascadeOrigin::Author) => Ordering::Greater,
                        (CascadeOrigin::User, CascadeOrigin::UserAgent) => Ordering::Less,
                        (CascadeOrigin::User, CascadeOrigin::User) => Ordering::Equal,
                        (CascadeOrigin::User, CascadeOrigin::Author) => Ordering::Greater,
                        (CascadeOrigin::Author, CascadeOrigin::UserAgent)
                        | (CascadeOrigin::Author, CascadeOrigin::User) => Ordering::Less,
                        (CascadeOrigin::Author, CascadeOrigin::Author) => Ordering::Equal,
                    }
                }
            }
        }

        if mem::discriminant(&self.inner_decl) == mem::discriminant(&other.inner_decl) {
            if self.important && !other.important {
                return Ordering::Greater;
            } else if !self.important && other.important {
                return Ordering::Less;
            } else if self.important && other.important {
                match cmp_important_origins(&self.origin, &other.origin) {
                    Ordering::Greater => return Ordering::Greater,
                    Ordering::Less => return Ordering::Less,
                    Ordering::Equal => return self.specificity.cmp(&other.specificity),
                }
            } else if !self.important && !other.important {
                return match cmp_important_origins(&self.origin, &other.origin) {
                    Ordering::Less => Ordering::Greater,
                    Ordering::Greater => Ordering::Less,
                    Ordering::Equal => return self.specificity.cmp(&other.specificity),
                };
            }
        }
        return Ordering::Equal;
    }
}

impl PartialOrd for ContextualPropertyDeclaration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for ContextualPropertyDeclaration {}
impl PartialEq for ContextualPropertyDeclaration {
    fn eq(&self, other: &Self) -> bool {
        return mem::discriminant(&self.inner_decl) == mem::discriminant(&other.inner_decl)
            && &self.origin == &other.origin;
    }
}

/// A declaration [importance][importance].
///
/// [importance]: https://drafts.csswg.org/css-cascade/#importance
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Importance {
    /// Indicates a declaration without `!important`.
    Normal,

    /// Indicates a declaration with `!important`.
    Important,
}

impl Importance {
    /// Return whether this is an important declaration.
    pub fn important(self) -> bool {
        match self {
            Importance::Normal => false,
            Importance::Important => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::style::properties::PropertyDeclaration;
    use crate::style::test_utils::font_size_px_or_panic;
    use crate::style::values::specified::length::*;

    use super::*;
    use crate::style::values::specified::Display;
    use crate::style::StylesheetOrigin;
    use std::clone::Clone;

    #[test]
    fn decl_cmp_specificity() {
        let zero_spec = ContextualPropertyDeclaration {
            inner_decl: PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(12.0)),
            ))),
            important: true,
            origin: CssOrigin::Inline,
            source_location: None,
            specificity: Specificity::new(0),
        };
        let mut one_thousand_spec = zero_spec.clone();
        one_thousand_spec.specificity = Specificity::new(1000);
        let mut two_thousand_spec = zero_spec.clone();
        two_thousand_spec.specificity = Specificity::new(2049);

        assert!(two_thousand_spec > one_thousand_spec);
        assert!(two_thousand_spec > zero_spec);
        assert!(one_thousand_spec > zero_spec);

        assert_eq!(
            two_thousand_spec.cmp(&two_thousand_spec.clone()),
            Ordering::Equal
        );
        assert_eq!(
            one_thousand_spec.cmp(&one_thousand_spec.clone()),
            Ordering::Equal
        );
        assert_eq!(zero_spec.cmp(&zero_spec.clone()), Ordering::Equal);
    }

    #[test]
    fn decl_cmp_importance_ordering() {
        let imp = ContextualPropertyDeclaration {
            inner_decl: PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(12.0)),
            ))),
            important: true,
            origin: CssOrigin::Inline,
            source_location: None,
            specificity: Specificity::new(0),
        };
        let mut not_imp = imp.clone();
        not_imp.important = false;

        assert!(imp > not_imp);
        assert!(not_imp < imp);
        assert_eq!(imp.cmp(&imp.clone()), Ordering::Equal);
        assert_eq!(not_imp.cmp(&not_imp.clone()), Ordering::Equal);
    }

    #[test]
    fn decl_cmp_both_important_sheet_origin() {
        let ua_decl = ContextualPropertyDeclaration {
            inner_decl: PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(12.0)),
            ))),
            important: true,
            origin: CssOrigin::Sheet(StylesheetOrigin {
                sheet_name: "file.css".to_owned(),
                cascade_origin: CascadeOrigin::UserAgent,
            }),
            source_location: None,
            specificity: Specificity::new(0),
        };
        let mut user_decl = ua_decl.clone();
        let mut author_decl = ua_decl.clone();
        user_decl.origin = CssOrigin::Sheet(StylesheetOrigin {
            sheet_name: "file.css".to_owned(),
            cascade_origin: CascadeOrigin::User,
        });
        author_decl.origin = CssOrigin::Sheet(StylesheetOrigin {
            sheet_name: "file.css".to_owned(),
            cascade_origin: CascadeOrigin::Author,
        });

        assert!(ua_decl > user_decl);
        assert!(ua_decl > author_decl);

        assert!(user_decl > author_decl);

        assert_eq!(ua_decl.cmp(&ua_decl.clone()), Ordering::Equal);
        assert_eq!(user_decl.cmp(&user_decl.clone()), Ordering::Equal);
        assert_eq!(author_decl.cmp(&author_decl.clone()), Ordering::Equal);
    }

    #[test]
    fn decl_cmp_both_unimportant_sheet_origin() {
        let ua_decl = ContextualPropertyDeclaration {
            inner_decl: PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(12.0)),
            ))),
            important: false,
            origin: CssOrigin::Sheet(StylesheetOrigin {
                sheet_name: "file.css".to_owned(),
                cascade_origin: CascadeOrigin::UserAgent,
            }),
            source_location: None,
            specificity: Specificity::new(0),
        };
        let mut user_decl = ua_decl.clone();
        let mut author_decl = ua_decl.clone();
        user_decl.origin = CssOrigin::Sheet(StylesheetOrigin {
            sheet_name: "file.css".to_owned(),
            cascade_origin: CascadeOrigin::User,
        });
        author_decl.origin = CssOrigin::Sheet(StylesheetOrigin {
            sheet_name: "file.css".to_owned(),
            cascade_origin: CascadeOrigin::Author,
        });

        assert!(author_decl > user_decl);
        assert!(author_decl > ua_decl);

        assert!(user_decl > ua_decl);

        assert_eq!(ua_decl.cmp(&ua_decl.clone()), Ordering::Equal);
        assert_eq!(user_decl.cmp(&user_decl.clone()), Ordering::Equal);
        assert_eq!(author_decl.cmp(&author_decl.clone()), Ordering::Equal);
    }

    #[test]
    fn decl_cmp_diff_prop_types_are_equal() {
        let font_size = ContextualPropertyDeclaration {
            inner_decl: PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(12.0)),
            ))),
            important: false,
            origin: CssOrigin::Inline,
            source_location: None,
            specificity: Specificity::new(0),
        };
        let display = ContextualPropertyDeclaration {
            inner_decl: PropertyDeclaration::Display(Display::Block),
            important: false,
            origin: CssOrigin::Inline,
            source_location: None,
            specificity: Specificity::new(0),
        };
        assert_eq!(font_size.cmp(&display), Ordering::Equal);
    }

    #[test]
    fn dedupes_and_takes_newest_prop() {
        let mut decl_block = PropertyDeclarationBlock::new();
        decl_block.add_declaration(
            PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(12.0)),
            ))),
            Importance::Normal,
        );
        decl_block.add_declaration(
            PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(16.0)),
            ))),
            Importance::Normal,
        );
        decl_block.add_declaration(
            PropertyDeclaration::FontSize(FontSize::Length(LengthPercentage::Length(
                NoCalcLength::Absolute(AbsoluteLength::Px(24.0)),
            ))),
            Importance::Normal,
        );
        assert_eq!(decl_block.declarations.len(), 1);
        assert_eq!(&24.0, font_size_px_or_panic(&decl_block.declarations[0]));
    }
}

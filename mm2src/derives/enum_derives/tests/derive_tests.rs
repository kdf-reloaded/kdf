//! Compile-time and runtime tests for all four derive macros.

use enum_derives::{EnumFromInner, EnumFromStringify, EnumFromTrait, EnumVariantList};
use std::fmt;

// ---------------------------------------------------------------------------
// EnumFromInner
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, EnumFromInner)]
enum Wrapper {
    #[from_inner]
    Text(String),
    #[from_inner]
    Num(i64),
    // Non-annotated variant: proves `EnumFromInner` generates `From` impls only
    // for `#[from_inner]` variants and leaves this one alone. Intentionally
    // never constructed, hence the allow.
    #[allow(dead_code)]
    Plain(Vec<u8>),
}

#[test]
fn from_inner_converts() {
    let w: Wrapper = String::from("hello").into();
    assert_eq!(w, Wrapper::Text("hello".into()));

    let w: Wrapper = 42i64.into();
    assert_eq!(w, Wrapper::Num(42));
}

// ---------------------------------------------------------------------------
// EnumFromStringify
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
struct CustomError(String);

impl fmt::Display for CustomError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "custom: {}", self.0) }
}

#[derive(Debug, PartialEq, EnumFromStringify)]
enum AppError {
    #[from_stringify("CustomError")]
    Stringified(String),
    // Non-annotated variant: proves `EnumFromStringify` leaves variants without
    // `#[from_stringify]` untouched. Intentionally never constructed, hence the
    // allow.
    #[allow(dead_code)]
    Other(String),
}

#[test]
fn from_stringify_converts() {
    let e = CustomError("boom".into());
    let app: AppError = e.into();
    assert_eq!(app, AppError::Stringified("custom: boom".into()));
}

// ---------------------------------------------------------------------------
// EnumFromTrait
// ---------------------------------------------------------------------------

trait WithMessage {
    fn with_message(msg: String) -> Self;
}

#[derive(Debug, PartialEq, EnumFromTrait)]
enum TraitErr {
    #[from_trait(WithMessage::with_message)]
    Msg(String),
}

#[test]
fn from_trait_implements() {
    let e = TraitErr::with_message("oops".into());
    assert_eq!(e, TraitErr::Msg("oops".into()));
}

// ---------------------------------------------------------------------------
// EnumVariantList
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, EnumVariantList)]
enum Color {
    Red,
    Green,
    Blue,
}

#[test]
fn variant_list_returns_all() {
    assert_eq!(Color::variant_list(), vec![Color::Red, Color::Green, Color::Blue]);
}

//! Fuiz question types and configuration
//!
//! This module contains the different question types supported by the Fuiz
//! game system. They fall into three families:
//!
//! - **Test knowledge** — [`multiple_choice`] (quiz / true-or-false),
//!   [`type_answer`], [`slider`], [`pin`] (pin answer), [`order`] (puzzle).
//! - **Collect opinions** — [`poll`], [`scale`] (agreement and NPS),
//!   [`pin`] (drop pin), [`free_text`] (word cloud and open ended),
//!   [`brainstorm`].
//! - **Present info** — [`info_slide`].
//!
//! Each question type has its own configuration, state management, and
//! message handling, built on the shared traits in [`common`].

pub mod brainstorm;
pub mod common;
pub mod config;
pub mod free_text;
pub mod info_slide;
pub mod media;
pub mod multiple_choice;
pub mod order;
pub mod pin;
pub mod poll;
pub mod scale;
pub mod slider;
pub mod type_answer;

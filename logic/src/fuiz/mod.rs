//! Fuiz question types and configuration
//!
//! This module contains the different question types supported by the Fuiz
//! game system, including multiple choice, type answer, and order questions.
//! Each question type has its own configuration, state management, and
//! message handling.

pub mod common;
pub mod config;
pub mod media;
pub mod multiple_choice;
pub mod order;
pub mod type_answer;

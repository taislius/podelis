//! The results pane: its keys and its mouse; the grid and how its rows are shown
//! (`grid`, `view`); editing a cell, adding and deleting rows, following a
//! foreign key; the plan and its lines; earlier runs; the messages; exports.

pub mod edit;
pub mod export;
pub mod export_form;
pub mod follow_fk;
pub mod grid;
mod keys;
pub mod messages;
mod mouse;
pub mod plan;
pub mod plan_lines;
pub mod rows;
pub mod runs;
pub mod view;

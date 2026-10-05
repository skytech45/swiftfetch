//! `SwiftFetch` site grabber/spider: BFS crawl from a seed URL with
//! include/exclude file-type filters, depth and page caps, robots.txt
//! compliance (default on) and per-host politeness delays.
//!
//! Implementation lands in Milestone 5 (Build Prompt §13.1; design contract
//! in docs/system-design.md §4.7). Politeness defaults are a hard guardrail —
//! never ship defaults that look like a crawl-abuse tool.

//! `SwiftFetch` networking: proxy support (HTTP, FTP, SOCKS, PAC file parsing)
//! and authentication (Basic, Digest, NTLM, Negotiate, Kerberos via system
//! SSPI on Windows / GSSAPI on Unix), cookie handling and TLS configuration.
//!
//! Implementation lands in Milestones 1 and 4 (Build Prompt §9.1, §12; design
//! contract in docs/system-design.md §4.5). Secrets never leave the OS
//! keyring path — see the security notes in docs/system-design.md.

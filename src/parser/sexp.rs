// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! S-expression tokenizer and parser for KiCad `.kicad_pcb` files.
//!
//! KiCad uses a Lisp-like S-expression format to store PCB design data.
//! This module provides a simple parser that converts the raw text into a tree
//! structure that can be navigated programmatically.
//!
//! # S-Expression Basics
//! An S-expression is either:
//! - An atom: a single word or quoted string (e.g., `hello`, `"hello world"`)
//! - A list: opening paren, zero or more S-expressions, closing paren (e.g., `(this is a list)`)
//!
//! Example from a KiCad file:
//! ```text
//! (segment (start 10.0 20.0) (end 30.0 40.0) (width 0.25) (layer "F.Cu"))
//! ```

use anyhow::{anyhow, Context, Result};

/// A single S-expression node, either an atom or a list.
#[derive(Debug, Clone, PartialEq)]
pub enum SexpNode {
    /// An atom: a string (unquoted identifier or quoted string)
    Atom(String),
    /// A list: a sequence of S-expressions
    List(Vec<SexpNode>),
}

impl SexpNode {
    /// Attempts to interpret this node as an atom, returning the string.
    ///
    /// # Example
    /// ```no_run
    /// if let Some(s) = node.as_atom() {
    ///     println!("Got atom: {}", s);
    /// }
    /// ```
    pub fn as_atom(&self) -> Option<&str> {
        match self {
            SexpNode::Atom(s) => Some(s),
            _ => None,
        }
    }

    /// Attempts to interpret this node as a list, returning the elements.
    pub fn as_list(&self) -> Option<&[SexpNode]> {
        match self {
            SexpNode::List(items) => Some(items),
            _ => None,
        }
    }

    /// Gets the first child of a list, or None if this node is not a list or is empty.
    #[allow(dead_code)]
    pub fn first(&self) -> Option<&SexpNode> {
        self.as_list().and_then(|items| items.first())
    }

    /// Gets the nth child of a list, or None if this node is not a list or doesn't have an nth element.
    pub fn nth(&self, index: usize) -> Option<&SexpNode> {
        self.as_list().and_then(|items| items.get(index))
    }

    /// Searches for the first child list whose first atom matches the given name.
    ///
    /// This is very common in KiCad parsing. For example, to find the `(start 10 20)` element
    /// in `(segment (start 10 20) (end 30 40))`, you'd call `get_child(node, "start")`.
    ///
    /// # Example
    /// ```no_run
    /// if let Some(start_node) = node.get_child("start") {
    ///     // start_node is now the (start 10 20) list
    /// }
    /// ```
    pub fn get_child(&self, name: &str) -> Option<&SexpNode> {
        self.as_list().and_then(|items| {
            items.iter().find(|item| {
                if let Some(child_list) = item.as_list() {
                    if let Some(first_atom) = child_list.first().and_then(|n| n.as_atom()) {
                        return first_atom == name;
                    }
                }
                false
            })
        })
    }
}

/// Tokenizer state machine for parsing S-expressions.
struct Tokenizer {
    /// The input string being tokenized
    input: Vec<char>,
    /// Current position in the input
    pos: usize,
}

impl Tokenizer {
    /// Creates a new tokenizer from a string.
    fn new(input: &str) -> Self {
        Tokenizer {
            input: input.chars().collect(),
            pos: 0,
        }
    }

    /// Returns the character at the current position without advancing.
    fn peek(&self) -> Option<char> {
        if self.pos < self.input.len() {
            Some(self.input[self.pos])
        } else {
            None
        }
    }

    /// Advances the position and returns the previous character.
    fn next(&mut self) -> Option<char> {
        if self.pos < self.input.len() {
            let ch = self.input[self.pos];
            self.pos += 1;
            Some(ch)
        } else {
            None
        }
    }

    /// Skips whitespace and comments.
    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.next();
            } else if ch == ';' {
                // Skip until end of line
                while let Some(c) = self.peek() {
                    self.next();
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Reads a quoted string (from opening quote to closing quote).
    fn read_quoted_string(&mut self) -> Result<String> {
        let mut result = String::new();
        self.next(); // Skip opening quote

        while let Some(ch) = self.next() {
            if ch == '"' {
                return Ok(result);
            } else if ch == '\\' {
                // Handle escape sequences
                if let Some(escaped) = self.next() {
                    match escaped {
                        'n' => result.push('\n'),
                        't' => result.push('\t'),
                        'r' => result.push('\r'),
                        '\\' => result.push('\\'),
                        '"' => result.push('"'),
                        _ => {
                            result.push('\\');
                            result.push(escaped);
                        }
                    }
                }
            } else {
                result.push(ch);
            }
        }

        Err(anyhow!("Unterminated quoted string"))
    }

    /// Reads an unquoted atom (sequence of non-whitespace, non-paren characters).
    fn read_unquoted_atom(&mut self) -> String {
        let mut result = String::new();

        while let Some(ch) = self.peek() {
            if ch.is_whitespace() || ch == '(' || ch == ')' {
                break;
            }
            result.push(ch);
            self.next();
        }

        result
    }

    /// Tokenizes the entire input and returns a vector of tokens.
    fn tokenize(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();

        loop {
            self.skip_whitespace();

            match self.peek() {
                None => break,
                Some('(') => {
                    tokens.push(Token::LParen);
                    self.next();
                }
                Some(')') => {
                    tokens.push(Token::RParen);
                    self.next();
                }
                Some('"') => {
                    let s = self.read_quoted_string()?;
                    tokens.push(Token::Atom(s));
                }
                Some(_) => {
                    let atom = self.read_unquoted_atom();
                    if !atom.is_empty() {
                        tokens.push(Token::Atom(atom));
                    }
                }
            }
        }

        Ok(tokens)
    }
}

/// A single token produced by the tokenizer.
#[derive(Debug, Clone)]
enum Token {
    /// Left paren `(`
    LParen,
    /// Right paren `)`
    RParen,
    /// An atom (unquoted identifier or quoted string)
    Atom(String),
}

/// Parser that builds an SexpNode tree from tokens.
struct Parser {
    /// The tokens produced by the tokenizer
    tokens: Vec<Token>,
    /// Current position in the token stream
    pos: usize,
}

impl Parser {
    /// Creates a new parser from a token stream.
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    /// Returns the current token without advancing.
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// Advances and returns the previous token.
    fn next(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let token = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(token)
        } else {
            None
        }
    }

    /// Parses a single S-expression node.
    fn parse_node(&mut self) -> Result<SexpNode> {
        match self.next() {
            Some(Token::Atom(s)) => Ok(SexpNode::Atom(s)),
            Some(Token::LParen) => self.parse_list(),
            Some(Token::RParen) => Err(anyhow!("Unexpected closing paren")),
            None => Err(anyhow!("Unexpected end of input")),
        }
    }

    /// Parses a list (starting after the opening paren has been consumed).
    fn parse_list(&mut self) -> Result<SexpNode> {
        let mut items = Vec::new();

        loop {
            match self.peek() {
                Some(Token::RParen) => {
                    self.next(); // Consume the closing paren
                    return Ok(SexpNode::List(items));
                }
                Some(_) => {
                    items.push(self.parse_node()?);
                }
                None => {
                    return Err(anyhow!("Unexpected end of input: expected closing paren"));
                }
            }
        }
    }

    /// Parses the entire token stream into a list of top-level nodes.
    fn parse(&mut self) -> Result<Vec<SexpNode>> {
        let mut nodes = Vec::new();

        loop {
            if self.peek().is_none() {
                break;
            }
            nodes.push(self.parse_node()?);
        }

        Ok(nodes)
    }
}

/// Parses a string containing S-expressions and returns a list of top-level nodes.
///
/// This is the main entry point for S-expression parsing.
///
/// # Example
/// ```no_run
/// let content = std::fs::read_to_string("board.kicad_pcb")?;
/// let nodes = parse_sexp(&content)?;
/// for node in nodes {
///     println!("Parsed: {:?}", node);
/// }
/// ```
pub fn parse_sexp(input: &str) -> Result<Vec<SexpNode>> {
    let mut tokenizer = Tokenizer::new(input);
    let tokens = tokenizer.tokenize().context("Tokenization failed")?;
    let mut parser = Parser::new(tokens);
    parser.parse().context("Parsing failed")
}

use std::collections::HashMap;

use super::{
    Error, Expr, Meta, OpChainKind, OpPart, OperatorBinding, OperatorDispatchKey, Parser, Span,
    Trivia, append_relocated_trivia, comment_span, inject_leading_trivia, token_kind_to_op_string,
};

struct Operand {
    expr: Expr,
    /// Explicit grouping and native suffixes erase a bare operator root.
    root: Option<OperatorDispatchKey>,
}

enum FixedStep {
    Operand,
    Suffixes,
    Done,
}

struct Frame {
    binding: OperatorBinding,
    key: OperatorDispatchKey,
    slot: usize,
    operands: Vec<Expr>,
    start: u32,
    leading: Vec<Trivia>,
}

impl Frame {
    fn following_word(&self) -> Option<&str> {
        match self.binding.pattern.get(self.slot + 1) {
            Some(OpPart::Token { content, .. }) => Some(content),
            _ => None,
        }
    }

    fn finish(self, end: u32, atomic: bool) -> Operand {
        let mut meta = Meta::new(Span::new(self.start, end));
        meta.leading_trivia = self.leading;
        Operand {
            expr: Expr::OpChain {
                occurrence: Default::default(),
                kind: OpChainKind::Normal {
                    pattern: self.binding.pattern,
                    slots: self.operands,
                },
                meta,
                ext: (),
            },
            root: (!atomic).then_some(self.key),
        }
    }
}

/// Only final-operand frames form the unfinished right spine. A frame still
/// awaiting a fixed word bounds that spine independently of operand validity.
#[derive(Default)]
struct Frames {
    stack: Vec<Frame>,
    boundaries: Vec<usize>,
    roots: HashMap<OperatorDispatchKey, Vec<usize>>,
    any_claims: Vec<usize>,
    self_claims: HashMap<OperatorDispatchKey, Vec<usize>>,
}

impl Frames {
    fn push(&mut self, frame: Frame) {
        let depth = self.stack.len();
        if frame.following_word().is_some() {
            self.boundaries.push(depth);
        } else {
            self.roots.entry(frame.key.clone()).or_default().push(depth);
            match frame.binding.pattern[frame.slot] {
                OpPart::SlotGreedy { .. } => self.any_claims.push(depth),
                OpPart::SlotRecursive { .. } => {
                    self.self_claims
                        .entry(frame.key.clone())
                        .or_default()
                        .push(depth);
                }
                OpPart::SlotPlain { .. } => {}
                OpPart::Token { .. } => unreachable!("an active fixed frame awaits a slot"),
            }
        }
        self.stack.push(frame);
    }

    fn pop(&mut self) -> Frame {
        let frame = self.stack.pop().expect("active fixed frame");
        if frame.following_word().is_some() {
            self.boundaries.pop();
        } else {
            pop_depth(&mut self.roots, &frame.key);
            match frame.binding.pattern[frame.slot] {
                OpPart::SlotGreedy { .. } => {
                    self.any_claims.pop();
                }
                OpPart::SlotRecursive { .. } => pop_depth(&mut self.self_claims, &frame.key),
                OpPart::SlotPlain { .. } => {}
                OpPart::Token { .. } => unreachable!("an active fixed frame awaits a slot"),
            }
        }
        frame
    }

    fn left_extent(&self, slot: &OpPart, key: &OperatorDispatchKey) -> usize {
        let lower = self.boundaries.last().map_or(0, |depth| depth + 1);
        match slot {
            OpPart::SlotPlain { .. } => self.stack.len(),
            OpPart::SlotRecursive { .. } => self
                .roots
                .get(key)
                .and_then(|depths| first_depth_at_or_after(depths, lower))
                .unwrap_or(self.stack.len()),
            OpPart::SlotGreedy { .. } => lower,
            OpPart::Token { .. } => unreachable!("a continuation starts with a slot"),
        }
    }

    fn opposing_claim(&self, lower: usize, key: &OperatorDispatchKey) -> bool {
        first_depth_at_or_after(&self.any_claims, lower).is_some()
            || self
                .self_claims
                .get(key)
                .and_then(|depths| first_depth_at_or_after(depths, lower))
                .is_some()
    }
}

fn pop_depth(index: &mut HashMap<OperatorDispatchKey, Vec<usize>>, key: &OperatorDispatchKey) {
    let depths = index.get_mut(key).expect("indexed active fixed frame");
    depths.pop();
    if depths.is_empty() {
        index.remove(key);
    }
}

fn first_depth_at_or_after(depths: &[usize], lower: usize) -> Option<usize> {
    depths
        .get(depths.partition_point(|depth| *depth < lower))
        .copied()
}

impl Parser<'_> {
    pub(super) fn fixed_operator_expr(&mut self) -> Result<Expr, Error> {
        self.fixed_operator_region(false)
    }

    pub(super) fn fixed_operator_primary(&mut self) -> Result<Expr, Error> {
        self.fixed_operator_region(true)
    }

    fn fixed_operator_region(&mut self, tight_receiver: bool) -> Result<Expr, Error> {
        let mut frames = Frames::default();
        let mut current: Option<Operand> = None;
        let mut next = FixedStep::Operand;
        loop {
            match next {
                FixedStep::Operand => {
                    if self.push_fixed_prefix(&mut frames) {
                        continue;
                    }
                    current = Some(self.fixed_native_operand(&frames)?);
                }
                FixedStep::Suffixes => {
                    let operand = current.take().expect("a completed fixed atom");
                    current = Some(Operand {
                        expr: self.expr_suffixes(operand.expr)?,
                        root: None,
                    });
                }
                FixedStep::Done => {
                    return Ok(current.take().expect("a completed expression").expr);
                }
            }
            next = self.fixed_operator_step(&mut frames, &mut current, tight_receiver)?;
        }
    }

    fn push_fixed_prefix(&mut self, frames: &mut Frames) -> bool {
        self.operator_cursor(true);
        let Some((binding, words)) = self.operator_scope.find_at(self, true) else {
            return false;
        };
        let leading = if frames.stack.is_empty() {
            Vec::new()
        } else {
            self.peek_leading_trivia()
        };
        let start = self.peek_span().start;
        for _ in 0..words {
            self.advance();
        }
        let key = OperatorDispatchKey::from_pattern(&binding.pattern);
        frames.push(Frame {
            binding,
            key,
            slot: words,
            operands: Vec::new(),
            start,
            leading,
        });
        true
    }

    fn fixed_native_operand(&mut self, frames: &Frames) -> Result<Operand, Error> {
        let leading = if frames.stack.is_empty() {
            Vec::new()
        } else {
            self.peek_leading_trivia()
        };
        let mut expr = self.expr_postfix()?;
        inject_leading_trivia(&mut expr, leading);
        Ok(Operand { expr, root: None })
    }

    // Grouping transitions return before native parsing recurses, keeping the
    // transition's expression temporaries off the native-expression stack.
    fn fixed_operator_step(
        &mut self,
        frames: &mut Frames,
        current: &mut Option<Operand>,
        tight_receiver: bool,
    ) -> Result<FixedStep, Error> {
        let mut operand = current
            .take()
            .expect("a native atom or completed fixed expression");
        if tight_receiver && frames.boundaries.is_empty() {
            while !frames.stack.is_empty() {
                operand = self.finish_open_frame(frames.pop(), operand)?;
            }
            if operand.root.is_some() {
                return Err(Error::parse(
                    operand.expr.span(),
                    "a reverse-call receiver requires an atom; parenthesize an operand-ending operator expression",
                ));
            }
            *current = Some(operand);
            return Ok(FixedStep::Done);
        }
        if let Some(&depth) = frames.boundaries.last() {
            let expected = frames.stack[depth]
                .following_word()
                .expect("indexed boundary");
            if self.peek().and_then(token_kind_to_op_string).as_deref() == Some(expected) {
                while frames.stack.len() > depth + 1 {
                    operand = self.finish_open_frame(frames.pop(), operand)?;
                }
                let mut frame = frames.pop();
                Self::check_fixed_operand(
                    &frame.binding.pattern[frame.slot],
                    &frame.key,
                    &operand,
                )?;
                let end = self.peek_span().end;
                self.consume_operator_run_after(&mut operand.expr, 1);
                frame.operands.push(operand.expr);
                frame.slot += 2;
                if frame.slot == frame.binding.pattern.len() {
                    *current = Some(frame.finish(end, true));
                    return Ok(if tight_receiver && frames.stack.is_empty() {
                        FixedStep::Done
                    } else {
                        FixedStep::Suffixes
                    });
                } else {
                    frames.push(frame);
                }
                return Ok(FixedStep::Operand);
            }
        }

        if !self.operator_scope.non_prefix.is_empty() {
            self.cursor_choices(super::CursorSlot::OperatorContinuation, &[]);
        }
        self.operator_cursor(false);
        if let Some((binding, words)) = self.operator_scope.find_at(self, false) {
            let key = OperatorDispatchKey::from_pattern(&binding.pattern);
            let extent = frames.left_extent(&binding.pattern[0], &key);
            if frames.opposing_claim(extent, &key) {
                return Err(self.err_here(
                    "opposing operator-chain slots claim this boundary; use parentheses to select the grouping",
                ));
            }
            // The declared extent is fixed before any crossed operand is
            // checked. A failed check never selects a shorter expression.
            while frames.stack.len() > extent {
                operand = self.finish_open_frame(frames.pop(), operand)?;
            }
            Self::check_fixed_operand(&binding.pattern[0], &key, &operand)?;
            let start = operand.expr.span().start;
            let end = self.peek_span().end;
            self.consume_operator_run_after(&mut operand.expr, words);
            let slot = 1 + words;
            let frame = Frame {
                binding,
                key,
                slot,
                operands: vec![operand.expr],
                start,
                leading: Vec::new(),
            };
            if slot == frame.binding.pattern.len() {
                *current = Some(frame.finish(end, true));
                return Ok(if tight_receiver && frames.stack.is_empty() {
                    FixedStep::Done
                } else {
                    FixedStep::Suffixes
                });
            } else {
                frames.push(frame);
            }
            return Ok(FixedStep::Operand);
        }

        while !frames.stack.is_empty() {
            operand = self.finish_open_frame(frames.pop(), operand)?;
        }
        *current = Some(operand);
        Ok(FixedStep::Done)
    }

    fn finish_open_frame(&self, mut frame: Frame, operand: Operand) -> Result<Operand, Error> {
        if let Some(expected) = frame.following_word() {
            return Err(self.err_here(format!(
                "expected `{expected}` to continue the `{}` operator pattern",
                frame.key.leading_run.join(""),
            )));
        }
        Self::check_fixed_operand(&frame.binding.pattern[frame.slot], &frame.key, &operand)?;
        let end = operand.expr.span().end;
        frame.operands.push(operand.expr);
        Ok(frame.finish(end, false))
    }

    fn check_fixed_operand(
        slot: &OpPart,
        key: &OperatorDispatchKey,
        operand: &Operand,
    ) -> Result<(), Error> {
        Self::ensure_value_operand(&operand.expr)?;
        let Some(root) = &operand.root else {
            return Ok(());
        };
        let allowed = match slot {
            OpPart::SlotPlain { .. } => false,
            OpPart::SlotRecursive { .. } => root == key,
            OpPart::SlotGreedy { .. } => true,
            OpPart::Token { .. } => unreachable!("fixed operand validation receives a slot"),
        };
        if allowed {
            return Ok(());
        }
        let word = key.leading_run.join("");
        let child = root.leading_run.join("");
        let message = if root == key {
            format!(
                "operator `{word}` is non-associative in this atom slot; use explicit parentheses around the operand"
            )
        } else {
            format!(
                "different operators (`{word}` and `{child}`) require explicit parentheses in this slot; operator dispatch is precedence-free"
            )
        };
        Err(Error::parse(operand.expr.span(), message))
    }

    fn consume_operator_run_after(&mut self, operand: &mut Expr, count: usize) {
        for _ in 0..count {
            if let Some(token) = self.cursor.peek_token()
                && token
                    .leading_trivia
                    .iter()
                    .any(|trivia| comment_span(trivia).is_some())
            {
                append_relocated_trivia(
                    &mut operand.meta_mut().trailing_trivia,
                    token.leading_trivia.clone(),
                );
            }
            self.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{CallArg, Expr, Item, OpChainKind, OpPart};
    use crate::pass::parser::{parse, parse_lazy};

    fn source(patterns: &[&str], expression: &str) -> String {
        let heads = patterns
            .iter()
            .map(|pattern| format!("op {pattern}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("module subject; import operators({heads}); fn result() -> . {{ {expression} }}")
    }

    fn shape(expr: &Expr) -> String {
        match expr {
            Expr::Path { segments, .. } => segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
            Expr::OpChain {
                kind: OpChainKind::Normal { pattern, slots },
                ..
            } => {
                let words = pattern
                    .iter()
                    .filter_map(|part| match part {
                        OpPart::Token { content, .. } => Some(content.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                format!(
                    "{words}({})",
                    slots.iter().map(shape).collect::<Vec<_>>().join(",")
                )
            }
            Expr::Call { callee, args, .. } => format!(
                "call({},{})",
                shape(callee),
                args.iter()
                    .map(|arg| match arg {
                        CallArg::Value(value) => shape(value),
                        _ => panic!("value-only test arguments"),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Expr::Ufcs {
                receiver,
                callee_segments,
                ..
            } => format!(
                "{}({})",
                callee_segments
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                shape(receiver)
            ),
            other => panic!("unexpected test expression {other:?}"),
        }
    }

    fn assert_tree(patterns: &[&str], expression: &str, expected: &str) {
        let source = source(patterns, expression);
        let module = parse(&source).unwrap_or_else(|error| panic!("{source}: {error:?}"));
        assert_eq!(
            module,
            parse_lazy(&source).unwrap().force_all().unwrap(),
            "{source}"
        );
        let Some(Item::FnDef(function)) = module.items.last() else {
            panic!("subject function");
        };
        assert_eq!(shape(&function.body), expected, "{source}");
    }

    #[test]
    fn fixed_slot_grouping_trees() {
        for (patterns, expression, expected) in [
            (vec!["! __"], "! !a", "!(!(a))"),
            (vec!["_ ?"], "a ? ?", "?(?(a))"),
            (vec!["_ + _", "_ ?"], "a ? + b", "+(?(a),b)"),
            (vec!["_ < _ >"], "a<b> <c>", "< >(< >(a,b),c)"),
            (vec!["_ * ___", "& _"], "a * &b", "*(a,&(b))"),
            (vec!["___ + _", "~ _"], "~a+b", "+(~(a),b)"),
            (vec!["_ + __"], "a+b+c", "+(a,+(b,c))"),
            (vec!["__ + _"], "a+b+c", "+(+(a,b),c)"),
            (vec!["_ <| _ |>"], "a<|b<|c|> |>", "<| |>(a,<| |>(b,c))"),
            (vec!["_ ? __ : _"], "a?b?c:d:e", "? :(a,? :(b,c,d),e)"),
            (
                vec!["_ ?! __ ? _"],
                "a ?! b ?! c ? d ? e",
                "?! ?(a,?! ?(b,c,d),e)",
            ),
            (
                vec!["_ <| ___ |>", "_ |> _", "_ + _"],
                "a<|b+c|>",
                "<| |>(a,+(b,c))",
            ),
            (
                vec!["_ <| ___ |>", "_ |> _"],
                "a<|(b|>c)|>",
                "<| |>(a,|>(b,c))",
            ),
            (vec!["_ + _", "_ ?"], "a?(x)+b", "+(call(?(a),x),b)"),
            (vec!["_ + _", "_ ?"], "b+a?(x)", "+(b,call(?(a),x))"),
            (vec!["_ + _"], "a+b.>f", "+(a,f(b))"),
            (vec!["~ ___", "_ ?"], "~a?(x)", "~(call(?(a),x))"),
            (vec!["~ ___", "_ ?"], "~a?.>f", "~(f(?(a)))"),
            (
                vec!["_ + _", "_ < _ >"],
                "a+b<c>(d)",
                "+(a,call(< >(b,c),d))",
            ),
            (
                vec!["_ + _", "_ ?", "___ < _ >"],
                "a+b?(x)<c>(d)",
                "call(< >(+(a,call(?(b),x)),c),d)",
            ),
            (
                vec!["_ + _", "_ ?", "___ < _ >"],
                "a+b? <c>(d)",
                "call(< >(+(a,?(b)),c),d)",
            ),
            (vec!["_ + _", "___ < _ >"], "a+(b<c>)", "+(a,< >(b,c))"),
            (
                vec!["_ $ ___", "___ < _ >"],
                "a $ (b < c >)",
                "$(a,< >(b,c))",
            ),
            (
                vec!["_ $ ___", "___ < _ >"],
                "(a $ b) < c >",
                "< >($(a,b),c)",
            ),
            (
                vec!["_ ? ___ : ___", "_ + _", "_ * _"],
                "a ? b + c : d * e",
                "? :(a,+(b,c),*(d,e))",
            ),
            (
                vec!["___ ? ___ : _", "_ + _", "_ * _"],
                "a + b ? c * d : e",
                "? :(+(a,b),*(c,d),e)",
            ),
            (
                vec!["_ ? __ : __"],
                "a ? b ? c : d : e",
                "? :(a,? :(b,c,d),e)",
            ),
            (
                vec!["_ ? __ : __"],
                "a ? b : c ? d : e",
                "? :(a,b,? :(c,d,e))",
            ),
            (
                vec!["___ ? __ : ___ @ _"],
                "a ? b ? c : d @ e : f @ g",
                "? : @(a,? : @(b,c,d,e),f,g)",
            ),
            (
                vec!["__ ? _ : _"],
                "a ? b : c ? d : e",
                "? :(? :(a,b,c),d,e)",
            ),
            (vec!["! _", "_ < _ >"], "f.<(!a) <b>", "< >(f(!(a)),b)"),
            (vec!["< _ >", "_ + _"], "f.< <a> + b", "+(f(< >(a)),b)"),
            (vec!["< _ >"], "f.< <a>.>g", "g(f(< >(a)))"),
            (vec!["_ < _ >"], "f.<a<b>", "< >(f(a),b)"),
            (vec!["_ ?"], "f.<a ?", "?(f(a))"),
            (
                vec!["_ ? _ : _", "_ + _", "___ < _ >"],
                "a ? b + c < k > : d",
                "? :(a,< >(+(b,c),k),d)",
            ),
        ] {
            assert_tree(&patterns, expression, expected);
        }
    }

    #[test]
    fn fixed_slot_grouping_rejections() {
        for (patterns, expression, diagnostic) in [
            (vec!["! _"], "! !a", "non-associative"),
            (vec!["_ * _", "& _"], "a * &b", "different operators"),
            (vec!["_ + _", "~ _"], "~a+b", "different operators"),
            (vec!["_ + _"], "a+b+c", "non-associative"),
            (vec!["_ + _", "_ * _"], "a+b*c", "different operators"),
            (vec!["___ + _", "~ ___"], "~a+b", "opposing"),
            (vec!["_ $ ___", "___ < _ >"], "a$b<c>", "opposing"),
            (
                vec!["_ + _", "! _", "___ < _ >"],
                "a + !b <c>(d)",
                "different operators",
            ),
            (
                vec!["_ ? __ : __", "! _"],
                "a ? !b : c",
                "different operators",
            ),
            (vec!["_ < _ >"], "a < A >", "value"),
            (vec!["_ < _ >"], "a < (A) >", "value"),
            (vec!["_ < _ >"], "a < F(A) >", "value"),
            (vec!["! _"], "f.<!a", "receiver requires an atom"),
            (
                vec!["_ ? _ : _", "_ + _", "___ < _ >"],
                "a ? b + (c < k >) : d",
                "different operators",
            ),
            (
                vec!["_ ? _ : _", "_ + _", "! _", "___ < _ >"],
                "a ? b + !c < k > : d",
                "different operators",
            ),
        ] {
            let source = source(&patterns, expression);
            let error = parse(&source).expect_err(&source);
            assert!(error.diag().1.contains(diagnostic), "{source}: {error:?}");
        }
    }

    #[test]
    fn value_only_slots_do_not_leak_through_native_wrappers() {
        for expression in ["! A", "! (A)", "! F(A)", "! (A.>f)", "keep(! A)"] {
            let source = source(&["! _"], expression);
            assert!(parse(&source).is_err(), "{source}");
            assert!(
                parse_lazy(&source).unwrap().force_all().is_err(),
                "{source}"
            );
        }
        for expression in ["! keep(A)", "! F.f(A)"] {
            let source = source(&["! _"], expression);
            let parsed = parse(&source).unwrap_or_else(|error| panic!("{source}: {error:?}"));
            assert_eq!(parse_lazy(&source).unwrap().force_all().unwrap(), parsed);
        }
    }

    #[test]
    fn fixed_pattern_definition_validation() {
        for (pattern, diagnostic) in [
            ("!", "value slot"),
            ("! ? _", "alternate"),
            ("_ _ + _", "adjacent slots"),
            ("_ + (_)", "operator-token quotation"),
            ("__ ?", "ineffective"),
            ("_ ? __ ? _", "ineffective"),
            ("___ < __ >", "ineffective"),
            ("__ ? __ ? _", "ineffective"),
            ("___ + ___", "both exposed"),
            ("__ + __", "both exposed"),
            ("___ + __", "both exposed"),
            ("__ + ___", "both exposed"),
        ] {
            for source in [
                format!("module x; op {pattern} {{ impl f; }};"),
                format!("module x; import ops(op {pattern});"),
            ] {
                let error = parse(&source).expect_err(&source);
                assert!(error.diag().1.contains(diagnostic), "{source}: {error:?}");
            }
        }
        for pattern in [
            "_ (=) _",
            "_ ? __ : __",
            "! __ : __",
            "___ < ___ >",
            "___ ? __ : ___ @ _",
            "__ ? ___ : _",
            "_ ? __ : ___",
        ] {
            parse(&format!("module x; op {pattern} {{ impl f; }};"))
                .unwrap_or_else(|error| panic!("{pattern}: {error:?}"));
        }
    }

    #[test]
    fn each_admitted_self_slot_has_a_bare_self_witness() {
        let mut checked = 0;
        for count in 1..=3 {
            for prefix in [false, true] {
                for equal_words in [false, true] {
                    for permissions in 0..3usize.pow(count) {
                        let mut rest = permissions;
                        let mut parts = Vec::new();
                        for slot in 0..count as usize {
                            if prefix || slot != 0 {
                                parts.push(
                                    if equal_words {
                                        "?"
                                    } else {
                                        ["?", ":", "@"][slot]
                                    }
                                    .to_owned(),
                                );
                            }
                            parts.push(["_", "__", "___"][rest % 3].to_owned());
                            rest /= 3;
                        }
                        let pattern = parts.join(" ");
                        // This generated domain is operand-ending. Repeated
                        // words exclude interior self slots, while its two
                        // exposed ends need an atom slot when both exist.
                        let has_word = prefix || count > 1;
                        let exposed_ends_allowed = prefix
                            || parts.first().is_some_and(|part| part == "_")
                            || parts.last().is_some_and(|part| part == "_");
                        let repeated_words_allowed = prefix
                            || !equal_words
                            || parts.iter().enumerate().all(|(index, part)| {
                                part != "__" || index == 0 || index == parts.len() - 1
                            });
                        let expected = has_word && exposed_ends_allowed && repeated_words_allowed;
                        let declaration = format!("module x; import ops(op {pattern});");
                        assert_eq!(parse(&declaration).is_ok(), expected, "{declaration}");
                        if !expected {
                            continue;
                        }
                        let atomized = parts
                            .iter()
                            .map(|part| if part.starts_with('_') { "x" } else { part })
                            .collect::<Vec<_>>()
                            .join(" ");
                        for (selected, part) in parts.iter().enumerate() {
                            if part != "__" {
                                continue;
                            }
                            let expression = parts
                                .iter()
                                .enumerate()
                                .map(|(index, part)| {
                                    if index == selected {
                                        atomized.as_str()
                                    } else if part.starts_with('_') {
                                        "x"
                                    } else {
                                        part
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ");
                            let witness = source(&[&pattern], &expression);
                            parse(&witness).unwrap_or_else(|error| {
                                panic!("missing useful witness {witness}: {error:?}")
                            });
                            let mut weakened = parts.clone();
                            weakened[selected] = "_".to_owned();
                            let negative = source(&[&weakened.join(" ")], &expression);
                            assert!(
                                parse(&negative).is_err(),
                                "ineffective accepted slot: {negative}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(
            checked > 50,
            "the generated domain must exercise multiple slot positions"
        );
    }
}

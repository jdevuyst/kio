# Design document

Trigger: editing `DESIGN.md`. AGENTS.md § Universal rules — "Project design
is approval-gated" governs changes to the document and departures from its
design. It protects the whole document, including the mechanisms described in
subsections.

## Goals first, then the design that delivers them

[`DESIGN.md`](../../DESIGN.md) explains what Kio sets out to deliver and how its
design serves those goals. Organize it by topic so the reader sees the purpose
and its realization together:

- Use a `##` heading that names the topic or goal.
- Open with prose explaining the commitment: the capability, guarantee, or
  experience Kio offers and why it matters to its users.
- Use `###` subsections for concrete mechanisms that deliver that commitment.
  Explain what each mechanism does and why it serves the goal. A topic may
  have several supporting mechanisms.

For example, inspection without a host implementation is a goal; normalization
and equivalence checking are two mechanisms serving it. Keep that relationship
visible without separate lists of commitments and choices or repeated category
labels. Explain the approach in agent guidance, not through instructions about
the document's layout in the public introduction.

## Scope and voice

Keep the introduction focused on Kio's purpose and the document's relationship
to the specifications. The design document carries goals and rationale;
specifications define exact accepted programs, semantics, and compatibility
contracts.

Lead with the problem or benefit before introducing machinery. Explain terms
such as dictionaries through the programming problem they address. Link to
specifications and worked examples for detail rather than repeating their
full contracts. A mechanism may serve several goals; use a cross-reference
when it avoids repeating the same explanation.

The document is a coherent design argument, not an exhaustive feature catalogue
or a compiler-maintenance checklist. State commitments directly. Include
technical boundaries where they clarify a guarantee, and keep permission rules
and agent workflow in the agent-guidance layer.

The goal/mechanism distinction explains rationale; it does not create different
levels of protection. Do not treat a subsection as optional, weaken a commitment
to match an implementation gap, or turn a design preference into authority to
change behavior. Follow the existing approval and contract-authority rules.

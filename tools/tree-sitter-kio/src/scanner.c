#include "tree_sitter/parser.h"
#include "doc_rules.h"
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum {
  STAR_OPEN, OPEN, STAR_CLOSE, CLOSE, EDITOR_BOUNDARY, EDITOR_BLOCK_BOUNDARY,
  FIXED_RUN, MINUS, UNOWNED_BRACKET_RUN, HEAD_OPEN, MALFORMED_HEAD_OPEN,
  PLACEHOLDER_OPEN, EDITOR_EOF,
  DOC_TEXT, DOC_FENCE, DOC_CODE, DOC_REFERENCE, DOC_DIRECTIVE,
  DOC_HEADING, DOC_EMPHASIS, DOC_STRONG, DOC_MARKER, DOC_LINK, DOC_ATTRIBUTE, DOC_KIO_CODE, DOC_KIO_SELF_CODE,
  DOC_LITERAL_FALLBACK, DOC_LINE_END, TOKEN_COUNT,
};

typedef struct { uint64_t length; uint8_t kind; } DocSegment;
typedef struct { uint64_t remaining; uint8_t width, kind; } DocStyle;
typedef struct {
  KioDocState fence;
  bool payload, heading, no_label_close, literal_fallback, fence_kio, markup_line, fence_self;
  uint8_t count, style_count, line_kind;
  uint64_t prefix_remaining, marker_remaining;
  DocSegment pending[5];
  DocStyle styles[4];
} Scanner;

typedef struct {
  uint32_t *data;
  size_t length, capacity;
} DocLine;

static bool doc_push(DocLine *line, uint32_t c) {
  if (line->length == line->capacity) {
    if (line->capacity > SIZE_MAX / (2 * sizeof(uint32_t))) return false;
    size_t capacity = line->capacity ? line->capacity * 2 : 64;
    void *data = realloc(line->data, capacity * sizeof(uint32_t));
    if (!data) return false;
    line->data = data;
    line->capacity = capacity;
  }
  line->data[line->length++] = c;
  return true;
}

static bool doc_read_line(TSLexer *lexer, DocLine *line) {
  while (!lexer->eof(lexer) && lexer->lookahead != '\n') {
    if (!doc_push(line, (uint32_t)lexer->lookahead)) return false;
    lexer->advance(lexer, false);
  }
  return true;
}

static bool doc_read_literal_line(TSLexer *lexer, DocLine *line, bool heading) {
  size_t threshold = 8;
  while (!lexer->eof(lexer) && lexer->lookahead != '\n') {
    if (!doc_push(line, (uint32_t)lexer->lookahead)) return false;
    lexer->advance(lexer, false);
    if (line->length >= threshold) {
      if (kio_doc_inline_decided(line->data, line->length, heading, false)) return true;
      threshold = threshold <= SIZE_MAX / 2 ? threshold * 2 : SIZE_MAX;
    }
  }
  return true;
}

static void doc_add(Scanner *state, uint64_t length, uint8_t kind) {
  if (length) state->pending[state->count++] = (DocSegment){length, kind};
}

static void doc_pop(Scanner *state) {
  state->count--;
  memmove(state->pending, state->pending + 1, state->count * sizeof(DocSegment));
  state->pending[state->count] = (DocSegment){0};
}

static uint8_t doc_kind(const Scanner *state) {
  if (state->style_count) return state->styles[state->style_count - 1].kind;
  return state->line_kind ? state->line_kind : DOC_TEXT;
}

static void doc_consume(Scanner *state, uint64_t length) {
  for (unsigned i = 0; i < state->style_count; i++) {
    uint64_t remaining = state->styles[i].remaining;
    state->styles[i].remaining = remaining > length ? remaining - length : 0;
  }
  while (state->style_count && !state->styles[state->style_count - 1].remaining) {
    state->styles[--state->style_count] = (DocStyle){0};
  }
}

static bool doc_prefix(TSLexer *lexer, unsigned remaining) {
  for (unsigned i = 0; i < remaining; i++) {
    if (lexer->lookahead != '/') return false;
    lexer->advance(lexer, false);
  }
  if (!lexer->eof(lexer) && lexer->lookahead != ' ' && lexer->lookahead != '\t' &&
      lexer->lookahead != '\r' && lexer->lookahead != '\n') return false;
  if (lexer->lookahead == ' ') lexer->advance(lexer, false);
  return true;
}

// A hidden opener is code only if its closer occurs in this physical doc run.
static bool doc_hidden_closes(TSLexer *lexer) {
  DocLine line = {0};
  bool closed = false;
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead == '\n') lexer->advance(lexer, false);
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\r') {
      lexer->advance(lexer, false);
    }
    if (lexer->eof(lexer) || !doc_prefix(lexer, 3)) break;
    line.length = 0;
    if (!doc_read_line(lexer, &line)) break;
    if (kio_doc_fence_facts(line.data, line.length).close_hidden) {
      closed = true;
      break;
    }
  }
  free(line.data);
  return closed;
}

static bool doc_start(Scanner *state, TSLexer *lexer, const bool *valid) {
  // The ordinary symbol scanner has consumed the first slash, not the second.
  if (!valid[DOC_TEXT] || !doc_prefix(lexer, 2)) return false;
  lexer->mark_end(lexer);
  DocLine line = {0};
  if (!doc_read_line(lexer, &line)) goto fail;
  size_t length = line.length;
  if (length && line.data[length - 1] == '\r' && lexer->lookahead == '\n') length--;
  const uint32_t empty = 0;
  const uint32_t *text = length ? line.data : &empty;
  size_t first = 0;
  while (first < length && kio_doc_whitespace(text[first])) first++;
  if (state->literal_fallback && !valid[DOC_LITERAL_FALLBACK]) goto fail;
  if (state->literal_fallback) {
    lexer->mark_end(lexer);
    state->payload = true;
    lexer->result_symbol = DOC_LITERAL_FALLBACK;
    free(line.data);
    return true;
  }
  bool quoted = false;
  for (size_t width; (width = kio_doc_quote_marker(text + first, length - first));) {
    quoted = true;
    first += width;
    while (first < length && kio_doc_whitespace(text[first])) first++;
  }
  state->heading = length && kio_doc_inline_heading(text + first, length - first);
  state->markup_line = state->heading || quoted;
  state->line_kind = state->heading ? DOC_HEADING : DOC_TEXT;
  state->prefix_remaining = first;
  state->marker_remaining = kio_doc_list_marker(text + first, length - first);
  if (!state->heading && !state->marker_remaining && kio_doc_thematic_break(text + first, length - first)) {
    state->marker_remaining = length - first;
  }
  if (state->count) {
    state->prefix_remaining = state->marker_remaining = 0;
    state->payload = true;
    lexer->result_symbol = DOC_TEXT;
    free(line.data);
    return true;
  }
  KioDocFenceFacts facts = kio_doc_fence_facts(text, length);
  bool special = false;
  for (size_t i = 0; i < length; i++) special |= kio_doc_inline_start(text[i]) || kio_doc_emphasis_marker(text[i]);
  bool definition = kio_doc_definition_bounds(text, length).found;
  bool whole = !state->fence.mode && !facts.open_hidden && !facts.open_visible && !special &&
    !state->style_count && !quoted && !state->heading && !state->marker_remaining;
  if (whole) lexer->mark_end(lexer);
  bool hidden_closed = state->fence.disabled || !facts.open_hidden || state->fence.mode != 0 || doc_hidden_closes(lexer);
  KioDocStep next = kio_doc_step(state->fence, facts, hidden_closed);
  int symbol = DOC_TEXT;
  switch (next.kind) {
    case KIO_DOC_OPEN: case KIO_DOC_CLOSE: case KIO_DOC_HIDDEN: symbol = DOC_FENCE; break;
    case KIO_DOC_CODE: symbol = state->fence_kio ? (state->fence_self ? DOC_KIO_SELF_CODE : DOC_KIO_CODE) : DOC_CODE; break;
    default: break;
  }
  state->fence = next.state;
  if (next.kind != KIO_DOC_PROSE && next.kind != KIO_DOC_UNTERMINATED) {
    state->no_label_close = false;
    state->style_count = 0;
    memset(state->styles, 0, sizeof(state->styles));
    state->prefix_remaining = state->marker_remaining = 0;
    state->line_kind = DOC_TEXT;
  }
  state->fence.active = true;
  state->payload = true;
  if (next.kind == KIO_DOC_OPEN || next.kind == KIO_DOC_HIDDEN) {
    size_t info = next.kind == KIO_DOC_HIDDEN ? KIO_DOC_HIDDEN_OPEN_WIDTH : facts.marker_end;
    state->fence_kio = kio_doc_is_kio_info(text + info, length - info);
    state->fence_self = state->fence_kio && kio_doc_self_harness_info(text + info, length - info);
    while (info < length && kio_doc_whitespace(text[info])) info++;
    while (info < length && !kio_doc_whitespace(text[info])) info++;
    size_t attribute = info, end = length;
    while (attribute < end && kio_doc_whitespace(text[attribute])) attribute++;
    while (end > attribute && kio_doc_whitespace(text[end - 1])) end--;
    doc_add(state, attribute, DOC_FENCE);
    doc_add(state, end - attribute, DOC_ATTRIBUTE);
    doc_add(state, length - end, DOC_TEXT);
  } else if (!whole && (symbol != DOC_TEXT || definition)) {
    doc_add(state, length, definition && symbol == DOC_TEXT ? DOC_LINK : (uint8_t)symbol);
    state->prefix_remaining = state->marker_remaining = 0;
  }
  if (next.kind == KIO_DOC_CLOSE) state->fence_kio = state->fence_self = false;
  lexer->result_symbol = DOC_TEXT;
  free(line.data);
  return true;
fail:
  free(line.data);
  return false;
}

// Inline lookahead uses only consecutive physical doc lines. A following batch
// fence bounds the prose block even when an inline delimiter is still open.
static bool doc_decided(const DocLine *text, bool heading, bool line_complete, bool emphasis) {
  return emphasis ? kio_doc_emphasis_decided(text->data, text->length, false) :
    kio_doc_inline_decided(text->data, text->length, heading, line_complete);
}

static bool doc_read_prose(TSLexer *lexer, DocLine *text, KioDocState fence, bool heading, bool emphasis, size_t limit) {
  // Geometric probes bound rescanning of a long current-line operand while
  // short, already-decided references do not inspect the rest of the doc run.
  size_t threshold = 32;
  while (!lexer->eof(lexer) && lexer->lookahead != '\n' && text->length < limit) {
    if (!doc_push(text, (uint32_t)lexer->lookahead)) return false;
    lexer->advance(lexer, false);
    if (text->length >= threshold) {
      if (doc_decided(text, heading, false, emphasis)) return true;
      threshold = threshold <= SIZE_MAX / 2 ? threshold * 2 : SIZE_MAX;
    }
  }
  if (text->length == limit) return true;
  if (lexer->lookahead == '\n' && text->length && text->data[text->length - 1] == '\r') text->length--;
  if (doc_decided(text, heading, true, emphasis) || (emphasis && heading)) return true;
  DocLine line = {0};
  bool hidden = false, closed = false;
  size_t hidden_start = 0;
  while (lexer->lookahead == '\n') {
    lexer->advance(lexer, false);
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\r') lexer->advance(lexer, false);
    if (!doc_prefix(lexer, 3)) break;
    line.length = 0;
    if (!doc_read_line(lexer, &line)) goto fail;
    if (lexer->lookahead == '\n' && line.length && line.data[line.length - 1] == '\r') line.length--;
    if (emphasis && kio_doc_inline_break(line.data, line.length)) break;
    KioDocFenceFacts facts = kio_doc_fence_facts(line.data, line.length);
    if (hidden) {
      closed |= facts.close_hidden;
    } else {
      KioDocStep step = kio_doc_step(fence, facts, true);
      if (step.kind == KIO_DOC_OPEN) break;
      if (step.kind == KIO_DOC_HIDDEN) { hidden = true; hidden_start = text->length; }
    }
    if (!doc_push(text, '\n')) goto fail;
    for (size_t i = 0; i < line.length && text->length < limit; i++) if (!doc_push(text, line.data[i])) goto fail;
    if (text->length == limit) break;
    // A hidden opener requires the same whole-run closing evidence as batch
    // parsing. Without a closer it disables subsequent fence interpretation.
    if (!hidden && text->length >= threshold) {
      if (doc_decided(text, heading, true, emphasis)) break;
      threshold = text->length <= SIZE_MAX / 2 ? text->length * 2 : SIZE_MAX;
    }
  }
  if (hidden && closed) text->length = hidden_start;
  free(line.data);
  return true;
fail:
  free(line.data);
  return false;
}

static bool doc_inline(Scanner *state, TSLexer *lexer, const bool *valid) {
  if (!valid[DOC_TEXT] || lexer->eof(lexer) || lexer->lookahead == '\n') return false;
  if (state->prefix_remaining) {
    size_t consumed = 0;
    int kind = DOC_TEXT;
    if (kio_doc_whitespace((uint32_t)lexer->lookahead)) {
      while (consumed < state->prefix_remaining && lexer->lookahead != '\n' &&
          kio_doc_whitespace((uint32_t)lexer->lookahead)) {
        lexer->advance(lexer, false);
        consumed++;
      }
    } else {
      uint32_t prefix[] = {(uint32_t)lexer->lookahead, 0};
      lexer->advance(lexer, false);
      prefix[1] = (uint32_t)lexer->lookahead;
      consumed = kio_doc_quote_marker(prefix, 2);
      if (!consumed) return false;
      if (consumed == 2) lexer->advance(lexer, false);
      kind = DOC_MARKER;
    }
    if (!valid[kind]) return false;
    state->prefix_remaining -= consumed;
    doc_consume(state, consumed);
    lexer->mark_end(lexer);
    lexer->result_symbol = kind;
    return true;
  }
  if (state->marker_remaining) {
    if (!valid[DOC_MARKER]) return false;
    uint64_t consumed = 0;
    while (consumed < state->marker_remaining && !lexer->eof(lexer) && lexer->lookahead != '\n') {
      lexer->advance(lexer, false);
      consumed++;
    }
    state->marker_remaining -= consumed;
    doc_consume(state, consumed);
    lexer->mark_end(lexer);
    lexer->result_symbol = DOC_MARKER;
    return consumed != 0;
  }
  if (state->style_count) {
    DocStyle top = state->styles[state->style_count - 1];
    if (top.remaining <= top.width) {
      if (!valid[top.kind]) return false;
      uint64_t consumed = 0;
      while (consumed < top.remaining && !lexer->eof(lexer) && lexer->lookahead != '\n') {
        lexer->advance(lexer, false);
        consumed++;
      }
      if (!consumed) return false;
      doc_consume(state, consumed);
      lexer->mark_end(lexer);
      lexer->result_symbol = top.kind;
      return true;
    }
  }
  uint8_t kind = doc_kind(state);
  uint64_t limit = state->style_count ? state->styles[state->style_count - 1].remaining - state->styles[state->style_count - 1].width : UINT64_MAX;
  uint64_t consumed = 0;
  while (!lexer->eof(lexer) && lexer->lookahead != '\n' &&
      lexer->lookahead != '\r' && consumed < limit &&
      !kio_doc_inline_start((uint32_t)lexer->lookahead) && !kio_doc_emphasis_marker((uint32_t)lexer->lookahead)) {
    lexer->advance(lexer, false);
    consumed++;
  }
  if (consumed) {
    if (!valid[kind]) return false;
    doc_consume(state, consumed);
    lexer->mark_end(lexer);
    lexer->result_symbol = kind;
    return true;
  }
  if (!kio_doc_inline_start((uint32_t)lexer->lookahead) && !kio_doc_emphasis_marker((uint32_t)lexer->lookahead)) {
    if (!valid[kind]) return false;
    bool cr = lexer->lookahead == '\r';
    lexer->advance(lexer, false);
    if (!cr || lexer->lookahead != '\n') doc_consume(state, 1);
    lexer->mark_end(lexer);
    lexer->result_symbol = kind;
    return true;
  }
  DocLine text = {0};
  if (!doc_push(&text, (uint32_t)lexer->lookahead)) return false;
  lexer->advance(lexer, false);
  lexer->mark_end(lexer);
  bool label = text.data[0] == KIO_DOC_LABEL_OPEN;
  bool emphasis = kio_doc_emphasis_marker(text.data[0]);
  if (emphasis) {
    size_t bounded = limit < SIZE_MAX ? (size_t)limit : SIZE_MAX;
    if (!doc_read_prose(lexer, &text, state->fence, state->markup_line, true, bounded)) { free(text.data); return false; }
  } else if (text.data[0] == KIO_DOC_INLINE_CODE_MARKER) {
    if (!doc_read_literal_line(lexer, &text, state->heading)) { free(text.data); return false; }
    size_t open = 0;
    while (open < text.length && text.data[open] == KIO_DOC_INLINE_CODE_MARKER) open++;
    if (kio_doc_inline_literal_end(text.data, text.length, 0, state->heading) == open) {
      if (!valid[DOC_LITERAL_FALLBACK]) { free(text.data); return false; }
      // This leaf explicitly records the line-oriented highlighting fallback.
      // A later closer cannot resume positive roles before the doc-run boundary.
      state->literal_fallback = true;
      state->style_count = 0;
      memset(state->styles, 0, sizeof(state->styles));
      lexer->mark_end(lexer);
      lexer->result_symbol = DOC_LITERAL_FALLBACK;
      free(text.data);
      return true;
    }
  } else if (label && state->no_label_close) {
    uint32_t prefix[] = {text.data[0], (uint32_t)lexer->lookahead};
    if (!kio_doc_bracket_bounds(prefix, 2, 0).found) {
      doc_consume(state, 1);
      lexer->result_symbol = kind;
      free(text.data);
      return true;
    }
    if (!doc_read_line(lexer, &text)) { free(text.data); return false; }
  } else if (!doc_read_prose(lexer, &text, state->fence, state->heading, false, SIZE_MAX)) {
    free(text.data); return false;
  }
  if (label && !state->no_label_close) {
    bool close = false;
    for (size_t i = 0; i < text.length; i++) close |= text.data[i] == KIO_DOC_LABEL_CLOSE;
    // Without this delimiter the conservative recognizer cannot decide early:
    // its lookahead reached the authenticated prose boundary. Tree-sitter tracks
    // that looked-ahead range, so later closer insertion invalidates the proof.
    state->no_label_close = !close;
  }
  size_t end = kio_doc_inline_literal_end(text.data, text.length, 0, state->heading);
  KioDocBounds link = kio_doc_link_bounds(text.data, text.length, 0);
  KioDocBounds bracket = kio_doc_bracket_bounds(text.data, text.length, 0);
  if (emphasis) {
    KioDocEmphasis style = kio_doc_emphasis_bounds(text.data, text.length, 0);
    if (style.width && state->style_count < 4) {
      kind = style.width == 2 ? DOC_STRONG : DOC_EMPHASIS;
      if (!valid[kind]) { free(text.data); return false; }
      state->styles[state->style_count++] = (DocStyle){style.end, (uint8_t)style.width, kind};
      doc_add(state, style.width - 1, kind);
    }
  } else if (end) {
    kind = text.data[0] == KIO_DOC_INLINE_CODE_MARKER ? DOC_CODE : DOC_MARKER;
    doc_add(state, end - 1, kind);
  } else if (link.found) {
    kind = DOC_LINK;
    doc_add(state, link.end, kind);
  } else if (bracket.found) {
    size_t after = bracket.end + (bracket.complete ? KIO_DOC_BRACKET_CLOSE_WIDTH : 0);
    size_t next = after;
    while (next < text.length && kio_doc_whitespace(text.data[next])) next++;
    if (bracket.complete && next < text.length &&
        (text.data[next] == KIO_DOC_LINK_OPEN || text.data[next] == KIO_DOC_LABEL_OPEN)) {
      doc_add(state, after - 1, DOC_TEXT);
    } else {
      size_t operand = bracket.start;
      doc_add(state, bracket.start - 1, DOC_TEXT);
      if (operand < bracket.end && text.data[operand] == KIO_DOC_DIRECTIVE_MARKER) {
        size_t directive_end = operand + 1;
        while (directive_end < bracket.end && kio_doc_ascii_alpha(text.data[directive_end])) directive_end++;
        doc_add(state, directive_end - operand, DOC_DIRECTIVE);
        operand = directive_end;
      }
      size_t a = operand, b = bracket.end;
      while (a < b && kio_doc_whitespace(text.data[a])) a++;
      while (b > a && kio_doc_whitespace(text.data[b - 1])) b--;
      doc_add(state, a - operand, DOC_TEXT);
      doc_add(state, b - a, DOC_REFERENCE);
      doc_add(state, after - b, DOC_TEXT);
    }
  }
  doc_consume(state, 1);
  lexer->result_symbol = kind;
  free(text.data);
  return true;
}

static bool symbol(int32_t c) {
  switch (c) {
    case '+': case '-': case '*': case '/': case '%': case '^': case '~':
    case '?': case '@': case '#': case '$': case '\\': case '\'': case '`':
    case '<': case '>': case '=': case '!': case '&': case '|': case ':':
    case '.': case '[': case ']': return true;
    default: return false;
  }
}

typedef struct {
  size_t count, opens, closes, stars, dots, slashes;
  int32_t first, second, last;
  bool comment_started;
} SymbolRun;

static bool trivia(int32_t c) {
  return c == ' ' || c == '\t' || c == '\r' || c == '\n';
}

// A stopped comment leaves lookahead on its second slash; mark_end remains
// before the first. The caller can inspect trivia without extending its token.
static bool next_symbol(TSLexer *lexer, SymbolRun *run) {
  int32_t c = lexer->lookahead;
  if (!symbol(c) || (c == '/' && run->slashes)) return false;
  lexer->advance(lexer, false);
  if (c == '/' && lexer->lookahead == '/') {
    run->comment_started = true;
    return false;
  }
  run->count++;
  if (run->count == 1) run->first = c;
  if (run->count == 2) run->second = c;
  run->opens += c == '['; run->closes += c == ']'; run->stars += c == '*';
  run->dots += c == '.'; run->slashes += c == '/'; run->last = c;
  return true;
}

static bool skip_comment(TSLexer *lexer) {
  lexer->advance(lexer, false); // second slash
  if (lexer->lookahead == '/') lexer->advance(lexer, false);
  if (!lexer->eof(lexer) && !trivia(lexer->lookahead)) return false;
  while (!lexer->eof(lexer) && lexer->lookahead != '\n') lexer->advance(lexer, false);
  return true;
}

static bool delimiter(const SymbolRun *run, bool opening) {
  return run->count >= 2 && !(run->first == '.' && run->dots == 1) &&
    (opening ? run->opens && !run->closes : run->closes && !run->opens);
}

// Header lookahead authenticates only the written split pair. Storage is local
// to this call; long openers grow without a language-level length limit.
static bool scan_head(Scanner *state, TSLexer *lexer, const bool *valid) {
  uint8_t inline_open[64], *open = inline_open;
  size_t capacity = sizeof(inline_open);
  SymbolRun opener = {0};
  bool result = false, matches = false;
  while (next_symbol(lexer, &opener)) {
    if (opener.count > capacity) {
      if (capacity > SIZE_MAX / 2) goto done;
      size_t grown = capacity * 2;
      uint8_t *buffer;
      if (open == inline_open) {
        buffer = malloc(grown);
        if (buffer) memcpy(buffer, inline_open, capacity);
      } else {
        buffer = realloc(open, grown);
      }
      if (!buffer) goto done;
      open = buffer;
      capacity = grown;
    }
    open[opener.count - 1] = (uint8_t)opener.last;
    lexer->mark_end(lexer);
  }
  if (!opener.count) {
    if (opener.comment_started) result = doc_start(state, lexer, valid);
    goto done;
  }
  if (!delimiter(&opener, true)) goto classified;
  bool separated = opener.comment_started;
  if (opener.comment_started && !skip_comment(lexer)) goto classified;
  for (;;) {
    while (trivia(lexer->lookahead)) {
      separated = true;
      lexer->advance(lexer, false);
    }
    SymbolRun closer = {0};
    bool equal = true;
    while (next_symbol(lexer, &closer)) {
      if (closer.count > opener.count) {
        equal = false;
      } else {
        uint8_t expected = open[opener.count - closer.count];
        if (expected == '[') expected = ']';
        if (closer.last != expected) equal = false;
      }
    }
    if (!closer.count && closer.comment_started) {
      separated = true;
      if (!skip_comment(lexer)) goto classified;
      continue;
    }
    matches = separated && delimiter(&closer, false) && equal && closer.count == opener.count;
    break;
  }
classified:
  if (matches && valid[HEAD_OPEN]) {
    lexer->result_symbol = HEAD_OPEN;
    result = true;
  } else if (!matches && valid[MALFORMED_HEAD_OPEN]) {
    lexer->result_symbol = MALFORMED_HEAD_OPEN;
    result = true;
  }
done:
  if (open != inline_open) free(open);
  return result;
}

void *tree_sitter_kio_external_scanner_create(void) { return calloc(1, sizeof(Scanner)); }
void tree_sitter_kio_external_scanner_destroy(void *p) { free(p); }
unsigned tree_sitter_kio_external_scanner_serialize(void *p, char *buffer) {
  Scanner *state = p;
  if (!state->fence.active && !state->payload && !state->count) return 0;
  buffer[0] = 3;
  buffer[1] = (char)state->fence.mode;
  buffer[2] = (char)state->fence.disabled;
  buffer[3] = (char)state->fence.active;
  for (unsigned i = 0; i < 8; i++) buffer[4 + i] = (char)(state->fence.width >> (i * 8));
  buffer[12] = (char)state->payload;
  buffer[13] = (char)state->heading;
  buffer[14] = (char)state->count;
  for (unsigned j = 0; j < 5; j++) {
    buffer[15 + j * 9] = (char)state->pending[j].kind;
    for (unsigned i = 0; i < 8; i++) buffer[16 + j * 9 + i] = (char)(state->pending[j].length >> (i * 8));
  }
  buffer[60] = (char)state->no_label_close;
  buffer[61] = (char)state->literal_fallback;
  buffer[62] = (char)state->fence_kio;
  buffer[63] = (char)state->style_count;
  buffer[64] = (char)state->line_kind;
  for (unsigned i = 0; i < 8; i++) {
    buffer[65 + i] = (char)(state->prefix_remaining >> (i * 8));
    buffer[73 + i] = (char)(state->marker_remaining >> (i * 8));
  }
  for (unsigned j = 0; j < 4; j++) {
    buffer[81 + j * 10] = (char)state->styles[j].width;
    buffer[82 + j * 10] = (char)state->styles[j].kind;
    for (unsigned i = 0; i < 8; i++) buffer[83 + j * 10 + i] = (char)(state->styles[j].remaining >> (i * 8));
  }
  buffer[121] = (char)state->markup_line;
  buffer[122] = (char)state->fence_self;
  return 123;
}
void tree_sitter_kio_external_scanner_deserialize(void *p, const char *buffer, unsigned length) {
  Scanner *state = p;
  *state = (Scanner){0};
  if (length != 123 || (unsigned char)buffer[0] != 3 || (unsigned char)buffer[1] > 2 ||
      (unsigned char)buffer[2] > 1 || (unsigned char)buffer[3] > 1 ||
      (unsigned char)buffer[12] > 1 || (unsigned char)buffer[13] > 1 || (unsigned char)buffer[14] > 5 || (unsigned char)buffer[60] > 1 || (unsigned char)buffer[61] > 1 ||
      (unsigned char)buffer[62] > 1 || (unsigned char)buffer[63] > 4 || (unsigned char)buffer[121] > 1 || (unsigned char)buffer[122] > 1 ||
      ((unsigned char)buffer[64] != 0 && (unsigned char)buffer[64] != DOC_TEXT && (unsigned char)buffer[64] != DOC_HEADING)) return;
  for (unsigned j = 0; j < (unsigned char)buffer[14]; j++) {
    if ((unsigned char)buffer[15 + j * 9] < DOC_TEXT || (unsigned char)buffer[15 + j * 9] > DOC_KIO_SELF_CODE) return;
  }
  for (unsigned j = 0; j < (unsigned char)buffer[63]; j++) {
    if ((unsigned char)buffer[81 + j * 10] < 1 || (unsigned char)buffer[81 + j * 10] > 2 ||
        ((unsigned char)buffer[82 + j * 10] != DOC_EMPHASIS && (unsigned char)buffer[82 + j * 10] != DOC_STRONG)) return;
  }
  state->fence.mode = (unsigned char)buffer[1];
  state->fence.disabled = buffer[2];
  state->fence.active = buffer[3];
  for (unsigned i = 0; i < 8; i++) state->fence.width |= (uint64_t)(unsigned char)buffer[4 + i] << (i * 8);
  state->payload = buffer[12];
  state->heading = buffer[13];
  state->count = (unsigned char)buffer[14];
  state->no_label_close = buffer[60];
  state->literal_fallback = buffer[61];
  state->fence_kio = buffer[62];
  state->style_count = (unsigned char)buffer[63];
  state->line_kind = (unsigned char)buffer[64];
  state->markup_line = buffer[121];
  state->fence_self = buffer[122];
  for (unsigned i = 0; i < 8; i++) {
    state->prefix_remaining |= (uint64_t)(unsigned char)buffer[65 + i] << (i * 8);
    state->marker_remaining |= (uint64_t)(unsigned char)buffer[73 + i] << (i * 8);
  }
  for (unsigned j = 0; j < state->style_count; j++) {
    state->styles[j].width = (unsigned char)buffer[81 + j * 10];
    state->styles[j].kind = (unsigned char)buffer[82 + j * 10];
    for (unsigned i = 0; i < 8; i++) state->styles[j].remaining |= (uint64_t)(unsigned char)buffer[83 + j * 10 + i] << (i * 8);
  }
  for (unsigned j = 0; j < state->count; j++) {
    state->pending[j].kind = (unsigned char)buffer[15 + j * 9];
    for (unsigned i = 0; i < 8; i++) state->pending[j].length |= (uint64_t)(unsigned char)buffer[16 + j * 9 + i] << (i * 8);
  }
}

static bool scan(Scanner *state, TSLexer *lexer, const bool *valid) {
  if (state->payload && state->count && lexer->lookahead != '\n') {
    if (!valid[state->pending[0].kind]) return false;
    uint64_t remaining = state->pending[0].length;
    uint64_t initial = remaining;
    bool consumed = false;
    while (remaining && !lexer->eof(lexer) && lexer->lookahead != '\n') {
      bool cr = lexer->lookahead == '\r';
      lexer->advance(lexer, false);
      consumed = true;
      if (!cr || lexer->lookahead != '\n') remaining--;
    }
    if (!consumed) return false;
    lexer->mark_end(lexer);
    lexer->result_symbol = state->pending[0].kind == DOC_TEXT ? doc_kind(state) : state->pending[0].kind;
    state->pending[0].length = remaining;
    if (!remaining) doc_pop(state);
    doc_consume(state, initial - remaining);
    return true;
  }
  if (state->payload && valid[DOC_LINE_END] && lexer->lookahead == '\n') {
    lexer->advance(lexer, false);
    lexer->mark_end(lexer);
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\r') {
      lexer->advance(lexer, false);
    }
    bool continues = doc_prefix(lexer, 3);
    if (state->count && !--state->pending[0].length) doc_pop(state);
    doc_consume(state, 1);
    state->payload = false;
    if (!continues) *state = (Scanner){0};
    lexer->result_symbol = DOC_LINE_END;
    return true;
  }
  if (state->payload && !lexer->eof(lexer)) return doc_inline(state, lexer, valid);
  while (trivia(lexer->lookahead)) {
    lexer->advance(lexer, true);
  }
  if (valid[EDITOR_EOF] && !(valid[STAR_OPEN] && valid[OPEN] && valid[STAR_CLOSE] && valid[CLOSE]) && lexer->eof(lexer)) {
    lexer->result_symbol = EDITOR_EOF;
    return true;
  }
  if ((valid[HEAD_OPEN] || valid[MALFORMED_HEAD_OPEN]) &&
      !(valid[STAR_OPEN] && valid[OPEN] && valid[STAR_CLOSE] && valid[CLOSE])) {
    return scan_head(state, lexer, valid);
  }
  if (valid[EDITOR_BLOCK_BOUNDARY] && !(valid[STAR_OPEN] && valid[OPEN] && valid[STAR_CLOSE] && valid[CLOSE]) &&
      (lexer->eof(lexer) || lexer->lookahead == '}')) {
    lexer->result_symbol = EDITOR_BLOCK_BOUNDARY;
    return true;
  }
  if (valid[EDITOR_BOUNDARY] && !(valid[STAR_OPEN] && valid[OPEN] && valid[STAR_CLOSE] && valid[CLOSE]) &&
      (lexer->eof(lexer) || lexer->lookahead == ')' || lexer->lookahead == '}')) {
    lexer->result_symbol = EDITOR_BOUNDARY;
    return true;
  }
  if (!valid[STAR_OPEN] && !valid[OPEN] && !valid[STAR_CLOSE] && !valid[CLOSE] &&
      !valid[FIXED_RUN] && !valid[MINUS] && !valid[UNOWNED_BRACKET_RUN] && !valid[PLACEHOLDER_OPEN] && !valid[DOC_TEXT]) return false;
  SymbolRun run = {0};
  lexer->mark_end(lexer);
  while (next_symbol(lexer, &run)) {
    if (run.first != '[' || run.opens != 1 || run.stars + 1 != run.count || run.count == 1 || !valid[STAR_OPEN]) {
      lexer->mark_end(lexer);
    }
  }
  if (!run.count && run.comment_started) return doc_start(state, lexer, valid);
  size_t count = run.count, opens = run.opens, closes = run.closes, stars = run.stars, dots = run.dots;
  int32_t first = run.first, second = run.second, last = run.last;
  if (!opens && !closes) {
    if (!count) return false;
    if (valid[PLACEHOLDER_OPEN] && count == 1 && first == '.') {
      if (lexer->lookahead == '_') lexer->advance(lexer, false);
      bool word_start = true, after_digit = false;
      while ((lexer->lookahead >= 'a' && lexer->lookahead <= 'z') ||
             (lexer->lookahead >= '0' && lexer->lookahead <= '9') || lexer->lookahead == '_') {
        int32_t c = lexer->lookahead;
        if (c == '_') {
          if (word_start) return false;
          word_start = true;
          after_digit = false;
        } else if (c >= '0' && c <= '9') {
          if (word_start) return false;
          after_digit = true;
        } else {
          if (after_digit) return false;
          word_start = false;
        }
        lexer->advance(lexer, false);
      }
      if (!word_start && !after_digit && lexer->lookahead == '.') {
        SymbolRun close = {0};
        while (next_symbol(lexer, &close)) {}
        if (close.count != 1 || (close.comment_started && !skip_comment(lexer))) return false;
        lexer->result_symbol = PLACEHOLDER_OPEN;
        return true;
      }
      return false;
    }
    // Builtin punctuation and UFCS/row arrows retain their grammar tokens.
    // Minus is shared by signed literals and ordinary operator expressions.
    if (count == 1 && (first == '/' || first == '.' ||
        first == '=' || first == '!' || first == '&' || first == '|' || first == ':')) return false;
    if (count == 1 && first == '-' && valid[MINUS]) { lexer->result_symbol = MINUS; return true; }
    if (count == 2 && first == '-' && last == '>') return false;
    if (first == '.' && dots == 1) {
      if ((count == 2 && (last == '>' || last == '<' || last == '?' || last == '!')) ||
          (count == 3 && (second == '>' || second == '<') && second == last)) return false;
    }
    if (valid[FIXED_RUN]) { lexer->result_symbol = FIXED_RUN; return true; }
    return false;
  }
  if (count >= 2 && !(first == '.' && dots < 2) && opens && !closes) {
    bool star = first == '[' && opens == 1 && stars + 1 == count;
    if (star && valid[STAR_OPEN]) { lexer->result_symbol = STAR_OPEN; return true; }
    if (!star && valid[OPEN]) { lexer->result_symbol = OPEN; return true; }
  }
  if (count >= 2 && !(first == '.' && dots < 2) && closes && !opens) {
    bool star = last == ']' && closes == 1 && stars + 1 == count;
    if (star && valid[STAR_CLOSE]) { lexer->result_symbol = STAR_CLOSE; return true; }
    if (!star && valid[CLOSE]) { lexer->result_symbol = CLOSE; return true; }
  }
  if (count && valid[UNOWNED_BRACKET_RUN]) { lexer->result_symbol = UNOWNED_BRACKET_RUN; return true; }
  return false;
}

bool tree_sitter_kio_external_scanner_scan(void *p, TSLexer *lexer, const bool *valid) {
  Scanner next = *(Scanner *)p;
  if (!scan(&next, lexer, valid) || !valid[lexer->result_symbol]) return false;
  *(Scanner *)p = next;
  return true;
}

#include "tree_sitter/parser.h"
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum { STAR_OPEN, OPEN, STAR_CLOSE, CLOSE, EDITOR_BOUNDARY, EDITOR_BLOCK_BOUNDARY, FIXED_RUN, MINUS, UNOWNED_BRACKET_RUN, HEAD_OPEN, MALFORMED_HEAD_OPEN, PLACEHOLDER_OPEN, EDITOR_EOF };

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
static bool scan_head(TSLexer *lexer, const bool *valid) {
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
  if (!opener.count) goto done;
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

void *tree_sitter_kio_external_scanner_create(void) { return NULL; }
void tree_sitter_kio_external_scanner_destroy(void *p) { (void)p; }
unsigned tree_sitter_kio_external_scanner_serialize(void *p, char *buffer) {
  (void)p; (void)buffer; return 0;
}
void tree_sitter_kio_external_scanner_deserialize(void *p, const char *buffer, unsigned length) {
  (void)p; (void)buffer; (void)length;
}

bool tree_sitter_kio_external_scanner_scan(void *p, TSLexer *lexer, const bool *valid) {
  (void)p;
  while (trivia(lexer->lookahead)) {
    lexer->advance(lexer, true);
  }
  if (valid[EDITOR_EOF] && !(valid[STAR_OPEN] && valid[OPEN] && valid[STAR_CLOSE] && valid[CLOSE]) && lexer->eof(lexer)) {
    lexer->result_symbol = EDITOR_EOF;
    return true;
  }
  if ((valid[HEAD_OPEN] || valid[MALFORMED_HEAD_OPEN]) &&
      !(valid[STAR_OPEN] && valid[OPEN] && valid[STAR_CLOSE] && valid[CLOSE])) {
    return scan_head(lexer, valid);
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
      !valid[FIXED_RUN] && !valid[MINUS] && !valid[UNOWNED_BRACKET_RUN] && !valid[PLACEHOLDER_OPEN]) return false;
  SymbolRun run = {0};
  lexer->mark_end(lexer);
  while (next_symbol(lexer, &run)) {
    if (run.first != '[' || run.opens != 1 || run.stars + 1 != run.count || run.count == 1 || !valid[STAR_OPEN]) {
      lexer->mark_end(lexer);
    }
  }
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

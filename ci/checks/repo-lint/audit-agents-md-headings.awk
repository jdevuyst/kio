# Validate AGENTS.md's outbound spec and topic section citations.
#
# Run from the repository root:
#   awk -f ci/checks/repo-lint/audit-agents-md-headings.awk AGENTS.md

function valid_spec_path(file, prefix, suffix, name, i, c) {
  prefix = "specs/"
  suffix = ".md"
  if (index(file, prefix) != 1 ||
      substr(file, length(file) - length(suffix) + 1) != suffix) {
    return 0
  }
  name = substr(file, length(prefix) + 1,
                length(file) - length(prefix) - length(suffix))
  if (name == "") return 0
  for (i = 1; i <= length(name); i++) {
    c = substr(name, i, 1)
    if (index("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789/-",
              c) == 0) return 0
  }
  return 1
}

function valid_topic_path(file, prefix, suffix, name, i, c) {
  prefix = "ai/topics/"
  suffix = ".md"
  if (index(file, prefix) != 1 ||
      substr(file, length(file) - length(suffix) + 1) != suffix) {
    return 0
  }
  name = substr(file, length(prefix) + 1,
                length(file) - length(prefix) - length(suffix))
  if (name == "") return 0
  for (i = 1; i <= length(name); i++) {
    c = substr(name, i, 1)
    if (index("abcdefghijklmnopqrstuvwxyz0123456789-", c) == 0) return 0
  }
  return 1
}

function path_kind(file) {
  if (valid_spec_path(file)) return "spec"
  if (valid_topic_path(file)) return "topic"
  return ""
}

function is_escaped(text, position, backslashes, i) {
  for (i = position - 1; i > 0 && substr(text, i, 1) == "\\"; i--) {
    backslashes++
  }
  return backslashes % 2
}

function marker_run_length(text, position, marker, run) {
  marker = substr(text, position, 1)
  while (substr(text, position + run, 1) == marker) run++
  return run
}

function has_closing_backtick_run(text, position, wanted, i, run) {
  for (i = position; i <= length(text);) {
    if (substr(text, i, 1) != "`") {
      i++
      continue
    }
    run = marker_run_length(text, i)
    if (run == wanted) return 1
    i += run
  }
  return 0
}

function opening_fence(line, indent, marker, run, rest) {
  while (substr(line, indent + 1, 1) == " ") indent++
  if (indent > 3) return 0
  marker = substr(line, indent + 1, 1)
  if (marker != "`" && marker != "~") return 0
  run = marker_run_length(line, indent + 1)
  if (run < 3) return 0
  rest = substr(line, indent + run + 1)
  if (marker == "`" && index(rest, "`") > 0) return 0
  return marker == "`" ? run : -run
}

function closes_fence(line, fence, indent, marker, run, rest, required) {
  while (substr(line, indent + 1, 1) == " ") indent++
  if (indent > 3) return 0
  marker = fence > 0 ? "`" : "~"
  if (substr(line, indent + 1, 1) != marker) return 0
  run = marker_run_length(line, indent + 1)
  required = fence > 0 ? fence : -fence
  if (run < required) return 0
  rest = substr(line, indent + run + 1)
  return rest ~ /^[[:blank:]]*$/
}

function normalized(text, result, i, c, run, code_ticks) {
  for (i = 1; i <= length(text);) {
    c = substr(text, i, 1)
    if (c != "`") {
      result = result c
      i++
      continue
    }
    run = marker_run_length(text, i)
    if (code_ticks) {
      if (run == code_ticks) code_ticks = 0
      else result = result substr(text, i, run)
    } else if (!is_escaped(text, i) &&
               has_closing_backtick_run(text, i + run, run)) {
      code_ticks = run
    } else {
      result = result substr(text, i, run)
    }
    i += run
  }
  return tolower(result)
}

function citation_tail_is_boundary(tail, code_ticks, first, rest) {
  if (code_ticks) return 0
  if (tail == "" || tail ~ /^[[:space:]]*$/) return 1
  if (index(tail, " — ") == 1) return 1
  first = substr(tail, 1, 1)
  rest = substr(tail, 2)
  if (index(".!?:;,", first) > 0 &&
      (rest == "" || rest ~ /^[[:space:]]/)) return 1
  if (tail ~ /^[[:space:]]+[|][[:space:]]*$/) return 1
  return 0
}

function citation_boundary_after_prefix(citation, prefix_length,
                                        i, c, run, emitted, code_ticks, tail) {
  for (i = 1; i <= length(citation) && emitted < prefix_length;) {
    c = substr(citation, i, 1)
    if (c != "`") {
      emitted++
      i++
      continue
    }
    run = marker_run_length(citation, i)
    if (code_ticks && run == code_ticks) {
      code_ticks = 0
      i += run
      continue
    }
    if (!code_ticks && !is_escaped(citation, i) &&
        has_closing_backtick_run(citation, i + run, run)) {
      code_ticks = run
      i += run
      continue
    }
    if (emitted + run >= prefix_length) {
      i += prefix_length - emitted
      emitted = prefix_length
      break
    }
    emitted += run
    i += run
  }
  if (emitted != prefix_length) return 0

  while (substr(citation, i, 1) == "`") {
    run = marker_run_length(citation, i)
    if (code_ticks && run == code_ticks) {
      code_ticks = 0
      i += run
      continue
    }
    if (!code_ticks && !is_escaped(citation, i) &&
        has_closing_backtick_run(citation, i + run, run)) {
      code_ticks = run
      i += run
      continue
    }
    break
  }
  tail = substr(citation, i)
  return citation_tail_is_boundary(tail, code_ticks)
}

function citation_starts_with_heading(citation, heading,
                                      normalized_citation,
                                      normalized_heading) {
  normalized_citation = normalized(citation)
  normalized_heading = normalized(heading)
  if (normalized_heading == "" ||
      index(normalized_citation, normalized_heading) != 1) return 0
  return citation_boundary_after_prefix(citation, length(normalized_heading))
}

function heading_prefix_before_dash(heading, i, c, run, code_ticks) {
  for (i = 1; i <= length(heading);) {
    c = substr(heading, i, 1)
    if (c == "`") {
      run = marker_run_length(heading, i)
      if (code_ticks && run == code_ticks) code_ticks = 0
      else if (!code_ticks && !is_escaped(heading, i) &&
               has_closing_backtick_run(heading, i + run, run)) {
        code_ticks = run
      }
      i += run
      continue
    }
    if (!code_ticks && substr(heading, i, length(" — ")) == " — ") {
      return substr(heading, 1, i - 1)
    }
    i++
  }
  return ""
}

function citation_matches_heading(citation, heading, prefix) {
  if (citation_starts_with_heading(citation, heading)) return 1
  prefix = heading_prefix_before_dash(heading)
  return prefix != "" && citation_starts_with_heading(citation, prefix)
}

function bounded_citation_matches_heading(citation, heading, prefix,
                                           normalized_citation) {
  normalized_citation = normalized(citation)
  if (normalized_citation == normalized(heading)) return 1
  prefix = heading_prefix_before_dash(heading)
  return prefix != "" && normalized_citation == normalized(prefix)
}

function bounded_citation_label(citation, label) {
  label = citation
  sub(/^[[:space:]]+/, "", label)
  sub(/[[:space:]]+$/, "", label)
  return label
}

function citation_label(citation, label, i, c, run, rest, code_ticks) {
  label = citation
  sub(/[[:space:]]+$/, "", label)
  if (label ~ /[[:space:]]+[|]$/) {
    sub(/[[:space:]]+[|]$/, "", label)
    sub(/[[:space:]]+$/, "", label)
  }
  for (i = 1; i <= length(label);) {
    c = substr(label, i, 1)
    if (c == "`") {
      run = marker_run_length(label, i)
      if (code_ticks && run == code_ticks) code_ticks = 0
      else if (!code_ticks && !is_escaped(label, i) &&
               has_closing_backtick_run(label, i + run, run)) {
        code_ticks = run
      }
      i += run
      continue
    }
    rest = substr(label, i + 1)
    if (!code_ticks && substr(label, i, length(" — ")) == " — ") {
      label = substr(label, 1, i - 1)
      break
    }
    if (!code_ticks && !is_escaped(label, i) &&
        index(".!?:;,", c) > 0 &&
        (rest == "" || rest ~ /^[[:space:]]/)) {
      label = substr(label, 1, i - 1)
      break
    }
    i++
  }
  sub(/[[:space:]]+$/, "", label)
  return label
}

function check_reference(kind, file, citation, raw_reference, bounded,
                         status, line, heading, found, fence, hashes, after) {
  status = (getline line < file)
  if (status < 0) {
    close(file)
    print "AGENTS.md cites missing " kind " file: " file \
          "  (from: " raw_reference ")"
    return
  }
  while (status > 0) {
    if (fence) {
      if (closes_fence(line, fence)) fence = 0
      status = (getline line < file)
      continue
    }
    fence = opening_fence(line)
    if (fence) {
      status = (getline line < file)
      continue
    }
    hashes = substr(line, 1, 1) == "#" ? marker_run_length(line, 1) : 0
    after = substr(line, hashes + 1, 1)
    if (hashes >= 1 && hashes <= 6 &&
        (after == "" || after ~ /[[:space:]]/)) {
      heading = substr(line, hashes + 1)
      sub(/^[[:space:]]+/, "", heading)
      sub(/[[:space:]]+$/, "", heading)
      if ((bounded && bounded_citation_matches_heading(citation, heading)) ||
          (!bounded && citation_matches_heading(citation, heading))) found = 1
    }
    status = (getline line < file)
  }
  close(file)
  if (!found) {
    print "AGENTS.md cites " kind " heading not found: " file " § " \
          (bounded ? bounded_citation_label(citation) : citation_label(citation))
  }
}

function check_once(kind, file, citation, raw_reference, bounded, key) {
  key = kind SUBSEP file SUBSEP citation SUBSEP bounded
  if (!(key in checked)) {
    checked[key] = 1
    check_reference(kind, file, citation, raw_reference, bounded)
  }
}

function labelled_link_close(text, i, c, run, code_ticks) {
  for (i = 1; i <= length(text);) {
    c = substr(text, i, 1)
    if (c == "`") {
      run = marker_run_length(text, i)
      if (code_ticks && run == code_ticks) code_ticks = 0
      else if (!code_ticks && !is_escaped(text, i) &&
               has_closing_backtick_run(text, i + run, run)) {
        code_ticks = run
      }
      i += run
      continue
    }
    if (!code_ticks && !is_escaped(text, i) &&
        substr(text, i, length("](")) == "](") return i
    i++
  }
  return 0
}

function check_labelled_spec_at(line, start, link, closing, label, delimiter,
                                file, citation) {
  link = substr(line, start + 1)
  closing = labelled_link_close(link)
  if (closing == 0) return
  label = substr(link, 1, closing - 1)
  delimiter = index(label, "` § ")
  if (delimiter == 0) return
  file = substr(label, 2, delimiter - 2)
  citation = substr(label, delimiter + length("` § "))
  if (valid_spec_path(file)) check_once("spec", file, citation, label, 1)
}

function check_plain_at(line, start, link, closing, file, after_link,
                        citation, kind) {
  link = substr(line, start + 1)
  closing = index(link, ")")
  if (closing == 0) return
  file = substr(link, 1, closing - 1)
  kind = path_kind(file)
  if (kind == "") return
  after_link = substr(link, closing + 1)
  if (index(after_link, " § ") != 1) return
  citation = substr(after_link, length(" § ") + 1)
  check_once(kind, file, citation, "(" file ") § " citation, 0)
}

function scan_source_line(line, i, c, run, code_ticks) {
  for (i = 1; i <= length(line);) {
    c = substr(line, i, 1)
    if (c == "`") {
      run = marker_run_length(line, i)
      if (code_ticks && run == code_ticks) {
        code_ticks = 0
      } else if (!code_ticks && !is_escaped(line, i) &&
                 has_closing_backtick_run(line, i + run, run)) {
        code_ticks = run
      }
      i += run
      continue
    }
    if (!code_ticks && !is_escaped(line, i)) {
      if (substr(line, i, length("[`specs/")) == "[`specs/") {
        check_labelled_spec_at(line, i)
      } else if (c == "(" &&
                 (substr(line, i + 1, length("specs/")) == "specs/" ||
                  substr(line, i + 1, length("ai/topics/")) == "ai/topics/")) {
        check_plain_at(line, i)
      }
    }
    i++
  }
}

{
  if (source_fence) {
    if (closes_fence($0, source_fence)) source_fence = 0
    next
  }

  source_opened_fence = opening_fence($0)
  if (source_opened_fence) {
    source_fence = source_opened_fence
    next
  }

  if ($0 == "## Universal rules") {
    source_section = "rules"
    next
  }
  if ($0 == "## Trigger table") {
    source_section = "triggers"
    next
  }
  if ($0 ~ /^##[[:space:]]/) {
    source_section = ""
    next
  }

  if (source_section == "rules" && $0 ~ /^- /) {
    source_opened_fence = opening_fence(substr($0, 3))
    if (source_opened_fence) {
      source_fence = source_opened_fence
      next
    }
    scan_source_line($0)
    next
  }
  if (source_section == "triggers" && $0 ~ /^\|/) {
    scan_source_line($0)
  }
}

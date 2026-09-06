#!/usr/bin/env ruby
# frozen_string_literal: true
#
# haml/1 — the divergence corpus's EXPECTATION generator.
#
# Runs the REAL `haml` gem's own `Haml::Parser` over every `*.haml` in this
# directory and writes `<name>.expected.json` beside it, in the projection
# shape `crates/kb-code-server/src/haml/projection.rs` produces from the
# first-party scanner. The Rust corpus test (`tests/haml_corpus.rs`) then
# diffs the two.
#
#   ***  THIS SCRIPT IS NEVER RUN BY CI, AND NEVER BY THE DAEMON.  ***
#
# It is run ONCE, by hand, on a developer box that has Ruby and the gem, and
# its OUTPUT is what is checked in. `ci-code` is pure Rust; kb-code-server's
# invariant 10 ("the daemon never spawns a non-git process") is untouched,
# and nothing in the build depends on Ruby existing anywhere.
#
# Regenerate (see CORPUS.md for the pinned gem version):
#
#   GEM_HOME=/tmp/haml-oracle gem install haml -v <version> --no-document
#   GEM_HOME=/tmp/haml-oracle ruby generate_expected.rb
#
# Keep this projection in LOCK-STEP with `projection.rs`. The three
# normalisations it applies (trimmed values, absent-and-empty inline are
# one thing, interpolated text is a `script` node) are documented in that
# module and in CORPUS.md; anything else that differs is a real divergence
# and the Rust test is supposed to go red.

require "haml"
require "json"

def quote_ruby(text)
  '"' + text.gsub(/[\\"]/) { |m| "\\" + m } + '"'
end

def inline(v)
  value = v[:value]
  return nil if value.nil?
  trimmed = value.to_s.strip
  return nil if trimmed.empty?
  v[:parse] ? { "script" => trimmed } : { "text" => trimmed }
end

def project(node)
  v = node.value.is_a?(Hash) ? node.value : {}
  row =
    case node.type
    when :doctype
      { "kind" => "doctype", "version" => v[:version], "doctype" => v[:type].to_s }
    when :tag
      {
        "kind" => "tag",
        "name" => v[:name],
        "attributes" => (v[:attributes] || {}).sort.to_h,
        "self_closing" => !!v[:self_closing],
        "inline" => inline(v),
      }
    when :plain
      { "kind" => "plain", "text" => v[:text].to_s.strip }
    when :script
      { "kind" => "script", "ruby" => v[:text].to_s.strip, "keyword" => v[:keyword] }
    when :silent_script
      { "kind" => "silent_script", "ruby" => v[:text].to_s.strip, "keyword" => v[:keyword] }
    when :haml_comment
      { "kind" => "haml_comment", "text" => v[:text].to_s.strip }
    when :comment
      { "kind" => "comment", "conditional" => v[:conditional], "text" => v[:text].to_s.strip }
    when :filter
      { "kind" => "filter", "name" => v[:name], "text" => v[:text].to_s.sub(/\s+\z/, "") }
    else
      raise "unhandled HAML node type #{node.type.inspect} — extend BOTH sides of the projection"
    end
  row.merge("children" => node.children.map { |c| project(c) })
end

dir = File.dirname(File.expand_path(__FILE__))
written = 0
Dir.glob(File.join(dir, "*.haml")).sort.each do |path|
  source = File.read(path)
  begin
    root = Haml::Parser.new({}).call(source)
  rescue Haml::Error => e
    # A corpus fixture MUST be legal HAML — the gem is the oracle, so a
    # fixture it rejects is a bug in the fixture, not a divergence to
    # record. Fail loudly, naming the file.
    abort "#{File.basename(path)}: the haml gem rejects this fixture: #{e.message.lines.first.strip}"
  end
  doc = { "nodes" => root.children.map { |c| project(c) } }
  out = path.sub(/\.haml\z/, ".expected.json")
  File.write(out, JSON.pretty_generate(doc) + "\n")
  written += 1
end
warn "wrote #{written} expectations with haml #{Haml::VERSION}"

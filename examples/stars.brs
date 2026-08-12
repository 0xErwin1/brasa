#!/usr/bin/env brasa
# The core scripting loop: read a file, walk JSON, shell out.
#
#   brasa examples/stars.brs examples/data/repos.json

import std::env
import std::fs
import std::io
import std::json
import std::proc

# Nothing here catches. A missing file, unreadable JSON or a missing
# `wc` are all worth stopping for, and an uncaught error already stops
# the run with its type and message — handling them here would only
# make the failure quieter.
def main()
  let args = env.args()
  if args.len() == 0
    io.eprint("usage: stars.brs <repos.json>\n")
    env.exit(2)
  end
  let file = args[0]

  let raw = fs.read(file)
  let data = json.parse(raw)

  # Indexing JSON is total: a missing key or a wrong-kinded node is
  # `None`, so each field ends in a `??` with its fallback in plain
  # sight instead of a guard further up.
  let mut popular = 0
  for repo in data["repos"].asArray() ?? []
    let stars = repo["stars"].asInt() ?? 0
    let archived = repo["archived"].asBool() ?? false

    if stars > 50 and !archived
      puts "#{repo["name"].asString() ?? "unknown"}: #{stars}"
      popular += 1
    end
  end
  puts "#{popular} popular repos"

  # The other half of scripting: hand data to a child process and read
  # its answer back as ordinary values.
  let counted = proc.run(["wc", "-l"], raw).stdout.trim()
  puts "#{counted} lines read from #{file}"
end

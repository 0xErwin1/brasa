#!/usr/bin/env brasa
# Summarize an nginx combined access log: how many requests came in, how
# many bytes went out, the status-class mix, the busiest paths and
# clients, and every endpoint that returned a server error.
#
#   brasa logstat.brs data/access.log
#   cat /var/log/nginx/access.log | brasa logstat.brs

import std::env
import std::fs
import std::io
import std::math

# A counted key, ready to be ranked. `Map.entries()` yields tuples and a
# tuple has no accessor, so a sortable row has to be a struct.
struct Tally
  key: string
  hits: int
end

let entryPattern = """^(\S+) \S+ \S+ \[[^\]]+\] "([A-Z]+) ([^ "]+)[^"]*" (\d{3}) (\d+|-)"""

# Reads the log from the first CLI argument, or from stdin when there is
# none, so the script works both as a file reader and as a pipe filter.
def source(): string
  let args = env.args()

  if args.len() == 0
    io.readAll()
  else
    fs.read(args[0])
  end
end

def bump(counts: Map<string, int>, key: string)
  counts.insert(key, (counts[key] ?? 0) + 1)
end

# Counters ordered by hits descending, ties broken by key. `sortBy` keys
# must be a single orderable value and it is stable, so the tie-break is
# a first pass by key.
def ranked(counts: Map<string, int>): Vector<Tally>
  let rows: Vector<Tally> = []

  for (key, hits) in counts.entries()
    rows.push(Tally { key: key, hits: hits })
  end

  rows.sortBy(|r| r.key).sortBy(|r| -r.hits)
end

def pad(n: int, width: int): string
  "#{n}".padStart(width, " ")
end

def percent(part: int, total: int): string
  let share = math.round(part.toFloat() / total.toFloat() * 1000.0) / 10.0

  "#{share}%"
end

def report(title: string, rows: Vector<Tally>, limit: int)
  puts ""
  puts title

  for i in 0..math.min(limit, rows.len())
    let row = rows[i]
    puts "  #{pad(row.hits, 4)}  #{row.key}"
  end
end

def main()
  let classes: Map<string, int> = {}
  let paths: Map<string, int> = {}
  let clients: Map<string, int> = {}
  let failing: Map<string, int> = {}

  let mut total = 0
  let mut skipped = 0
  let mut bytes = 0

  for line in source().lines()
    if line.trim() == ""
      continue
    end

    let caps: Vector<string> = line.captures(entryPattern) ?? []

    if caps.len() == 0
      skipped += 1
      continue
    end

    let client = caps[1]
    let method = caps[2]
    let path = caps[3].split("?")[0]
    let status = caps[4]

    total += 1
    bytes += caps[5].toInt() catch (e)
      string.ParseError => 0
    end

    bump(classes, status.slice(0, 1) + "xx")
    bump(paths, path)
    bump(clients, client)

    if status.startsWith?("5")
      bump(failing, "#{method} #{path}")
    end
  end

  puts "parsed #{total} requests, skipped #{skipped} unparsable lines"
  puts "bytes served: #{bytes}"

  puts ""
  puts "status classes:"

  for class in classes.keys().sort()
    let hits = classes[class] ?? 0
    puts "  #{class}  #{pad(hits, 4)}  #{percent(hits, total)}"
  end

  report("top paths:", ranked(paths), 5)
  report("top clients:", ranked(clients), 3)

  if failing.len() == 0
    puts ""
    puts "no server errors"
  else
    report("server errors by endpoint:", ranked(failing), failing.len())
  end
end

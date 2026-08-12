#!/usr/bin/env brasa
# Audit every `flake.lock` under a directory tree: which inputs a flake
# pins, where they come from, when they were locked, which ones are not
# pinned to a revision, and which ones are locked twice to the same
# revision (a `follows` opportunity).
#
#   brasa lockaudit.brs           # audits the current directory
#   brasa lockaudit.brs ~/dev

import std::env
import std::fs
import std::json
import std::math
import std::time

struct Input
  name: string
  rev: string
  date: string
  source: string
end

struct Duplicate
  rev: string
  names: Vector<string>
end

let pruned = Set([".git", ".direnv", "target", "result", "node_modules"])

# `fs.isDir?` follows symlinks, so a link pointing back at an ancestor
# would make the descent recurse until the stack overflows. A depth bound
# is the cheap fix: `fs.abs` is purely lexical, so a visited set built
# from it would not even recognize the revisited directory.
let maxDepth = 32

# `fs.ls` on an unreadable directory is a dead end, not a failure of the
# audit, so it degrades to "no entries".
def entries(dir: string): Vector<string>
  let none: Vector<string> = []

  fs.ls(dir) catch (e)
    fs.Denied => none
    fs.IoError => none
  end
end

# The descent is hand-rolled for reasons that no longer both hold:
# `fs.walk` gained a prune list (BRS-51) and `fs.tryWalk` gained
# tolerance for unreadable directories (BRS-66), so this could now be
# one call plus a filter. It is left as written because it is the
# pinned example of a hand-rolled traversal, and because its depth
# limit has no `walk` equivalent.
def collectLocks(dir: string, found: Vector<string>, depth: int)
  if depth > maxDepth
    return
  end

  let lock = fs.join(dir, "flake.lock")

  if fs.isFile?(lock)
    found.push(lock)
  end

  for name in entries(dir)
    let child = fs.join(dir, name)

    if !pruned.has?(name) && fs.isDir?(child)
      collectLocks(child, found, depth + 1)
    end
  end
end

def originOf(locked: Option<Json>, original: Option<Json>): string
  let kind = locked["type"].asString() ?? "?"

  if kind == "github" || kind == "gitlab"
    let owner = original["owner"].asString() ?? locked["owner"].asString() ?? "?"
    let repo = original["repo"].asString() ?? locked["repo"].asString() ?? "?"

    "#{kind}:#{owner}/#{repo}"
  else
    "#{kind}:#{locked["url"].asString() ?? original["url"].asString() ?? "?"}"
  end
end

def lockedDate(locked: Option<Json>): string
  let stamp = locked["lastModified"].asInt() ?? 0

  if stamp == 0
    "?"
  else
    time.iso(stamp * 1000).slice(0, 10)
  end
end

# An input entry whose value is an array is a `follows` link rather than
# a node reference.
def followsIn(node: Json): int
  let noInputs: Map<string, Json> = {}
  let mut count = 0

  for (_, value) in node["inputs"].asObject() ?? noInputs
    count += match value.asArray()
      Some(_) => 1
      None => 0
    end
  end

  count
end

# The flake's own inputs, as opposed to every node in the graph: a
# transitively-resolved dependency is a node too, but nothing the root
# flake declares.
def directInputs(nodes: Map<string, Json>): int
  let noInputs: Map<string, Json> = {}
  let inputs = nodes["root"]["inputs"].asObject() ?? noInputs

  inputs.len()
end

def widest(rows: Vector<Input>): int
  rows.map(|r| r.name.len()).reduce(0, |widest, len| math.max(widest, len))
end

def duplicates(revs: Map<string, Vector<string>>): Vector<Duplicate>
  let dupes: Vector<Duplicate> = []

  for (rev, names) in revs.entries()
    if names.len() > 1
      dupes.push(Duplicate { rev: rev, names: names })
    end
  end

  dupes.sortBy(|d| d.rev)
end

# A count with its noun, pluralized by the plain English rule. Written
# out because there is no formatting helper in the stdlib to lean on.
def count(n: int, noun: string): string
  if n == 1
    "#{n} #{noun}"
  else
    "#{n} #{noun}s"
  end
end

def audit(path: string, root: string)
  let data = json.parse(fs.read(path))
  let noNodes: Map<string, Json> = {}
  let nodes = data["nodes"].asObject() ?? noNodes

  let mut follows = 0

  for (_, node) in nodes
    follows += followsIn(node)
  end

  let shown = if path.startsWith?("#{root}/")
    path.slice(root.len() + 1, path.len())
  else
    path
  end

  puts "#{shown}  (lock version #{data["version"].asInt() ?? 0}, #{count(directInputs(nodes), "direct input")}, #{count(nodes.len() - 1, "locked node")}, #{count(follows, "follows edge")})"

  let revs: Map<string, Vector<string>> = {}
  let rows: Vector<Input> = []
  let unpinned: Vector<string> = []

  for (name, node) in nodes
    if name == "root"
      continue
    end

    let locked = node["locked"]
    let rev = locked["rev"].asString() ?? ""

    if rev == ""
      unpinned.push(name)
      continue
    end

    rows.push(Input {
      name: name,
      rev: rev.slice(0, 7),
      date: lockedDate(locked),
      source: originOf(locked, node["original"]),
    })

    let seen: Vector<string> = revs[rev] ?? []
    seen.push(name)
    revs.insert(rev, seen)
  end

  let width = widest(rows)

  for row in rows
    puts "  #{row.name.padEnd(width, " ")}  #{row.rev}  #{row.date}  #{row.source}"
  end

  if unpinned.len() > 0
    puts "  unpinned inputs: #{unpinned.join(", ")}"
  end

  let dupes = duplicates(revs)

  if dupes.len() == 0
    puts "  no duplicated revisions"
  else
    puts "  duplicated revisions:"

    for dupe in dupes
      puts "    #{dupe.rev.slice(0, 7)}  #{dupe.names.join(" ")}"
    end
  end
end

def main()
  let root = env.args().first() ?? env.cwd()
  let locks: Vector<string> = []

  collectLocks(root, locks, 0)

  if locks.len() == 0
    puts "no flake.lock under #{root}"
    return
  end

  let mut first = true

  for lock in locks
    if !first
      puts ""
    end

    first = false
    audit(lock, root)
  end
end

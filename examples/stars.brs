#!/usr/bin/env brasa
# Stdlib preview (runs after M4): the bash/Python replacement in action.
# Reads a JSON file, filters it, and shells out — the core scripting loop.

import std::fs
import std::json
import std::proc

def main()
  let raw = fs.read("repos.json") catch (e)
    fs.NotFound => "[]"
  end

  let data = json.parse(raw)
  let mut count = 0

  for repo in data["repos"] ?? json.parse("[]")
    let stars = repo["stars"] ?? 0
    if stars > 50
      puts "#{repo["name"] ?? "unknown"}: #{stars}"
      count += 1
    end
  end

  puts "#{count} popular repos"

  let branch = proc.run(["git", "branch", "--show-current"]).stdout.trim()
  puts "current branch: #{branch}"
end

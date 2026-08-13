# A CLI-shaped script signalling failure by status, not by crashing:
# nothing is written to stderr and no banner appears.
import std::env
import std::io

io.eprint("usage: exit_status <path>\n")
puts "checked 3 things"
env.exit(4)
puts "unreachable"

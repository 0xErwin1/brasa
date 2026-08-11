# String handling: methods, interpolation, raw strings, chars.

let line = "  Hello, Brasa World  "

puts line.trim()
puts line.trim().toUpper()
puts line.trim().replace("World", "Script")
puts "words: #{line.trim().split(" ").len()}"
puts "starts? #{line.trim().startsWith?("Hello")}"

# Nested interpolation.
let items = ["a", "b", "c"]
puts "joined: #{items.map(|s| "<#{s}>").join(", ")}"

# Raw strings: multiline, no escapes, interpolation still works.
let name = "brasa"
let snippet = """
line one \n stays literal
project: #{name}
"""
puts snippet

# Chars are unicode scalars.
for c in "ñandú".chars()
  puts c
end

# Parsing throws; catch gives the fallback.
let n = "not-a-number".toInt() catch (e)
  string.ParseError => -1
end
puts "parsed: #{n}"

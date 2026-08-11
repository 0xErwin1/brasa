# Golden: vector method chains, map insertion-order iteration and
# methods, shared-reference mutation observability, closure capture
# semantics, and string methods.

let nums = [5, 3, 8, 1, 9, 2]
let processed = nums.filter(|n| n % 2 == 1).map(|n| n * 10).sortBy(|n| n)
puts processed
puts "len: #{processed.len()}"
puts "first: #{processed.first() ?? -1}"
puts nums.reverse()
puts nums.contains?(8)

# Shared references: v2 aliases v, mutation is visible both ways.
let v = [1, 2]
let v2 = v
v2.push(3)
puts v
v.push(4)
puts v2

let words = ["fuego", "brasa", "ceniza"]
puts words.sortBy(|w| w).join(", ")

# Closures capture their environment at creation.
def makeAdder(k: int): (int) -> int
  |n| n + k
end
let add5 = makeAdder(5)
puts add5(10)

def captureLocal(): string
  let mut local = 1
  let show = |unused: int| "captured #{local}"
  local = 2
  show(0)
end
puts captureLocal()

# Captured heap values stay shared.
def captureHeap(): Vector<int>
  let inner = [1, 2]
  let grow = |n: int| inner.push(n)
  grow(3)
  inner
end
puts captureHeap()

let stock: Map<string, int> = { "ember": 3, "ash": 7 }
stock.insert("smoke", 1)
for (name, count) in stock
  puts "#{name} -> #{count}"
end
puts stock.keys()
puts stock["ash"] ?? 0
puts stock["lava"] ?? 0
puts stock.has?("ember")
puts stock.remove("ash") ?? -1
puts stock.len()
puts stock

let line = "  Brasa glows  "
puts line.trim().toUpper()
puts line.trim().split(" ").len()
puts "ñandú".len()
puts "abc".slice(1, 3)
puts "banana".count("an")
puts "brasa".find("as") ?? -1
puts "42".toInt() ?? -1
puts "nope".toInt() ?? -1

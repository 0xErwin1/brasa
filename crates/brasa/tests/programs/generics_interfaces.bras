# Golden: structural interfaces as generic constraints — a named
# interface, an inline one, one body reached at several concrete types,
# a constraint method taking arguments, a builtin receiver satisfying a
# user interface, and the universal toString on a generic value.

interface Labeled
  def label(self): string
end

interface Adder
  def add(self, other: Self): int
end

interface Lengthy
  def len(self): int
end

struct Tag
  name: string

  def label(self): string
    "tag:#{self.name}"
  end
end

struct Counter
  n: int

  def label(self): string
    "count:#{self.n}"
  end

  def add(self, other: Counter): int
    self.n + other.n
  end

  def toString(self): string
    "Counter(#{self.n})"
  end
end

struct Box<T>
  item: T

  def label(self): string
    "box"
  end
end

def show<T: Labeled>(v: T): string
  v.label()
end

def shout<T: { def label(self): string }>(v: T): string
  v.label().toUpper()
end

def twice<T: Labeled>(v: T): string
  "#{v.label()}/#{v.label()}"
end

def sum<T: Adder>(a: T, b: T): int
  a.add(b)
end

def size<T: Lengthy>(v: T): int
  v.len()
end

def render<T>(v: T): string
  v.toString()
end

def labels<T: Labeled>(v: T, times: int): string
  let out: Vector<string> = []
  for i in 0..times
    out.push(v.label())
  end
  out.join(",")
end

let tag = Tag { name: "x" }
let counter = Counter { n: 2 }

puts show(tag)
puts show(counter)
puts show(Box { item: 1 })
puts shout(tag)
puts twice(counter)
puts sum(counter, Counter { n: 40 })
puts size("abc")
puts size([1, 2])
puts render(tag)
puts render(counter)
puts render(7)
puts labels(tag, 3)

let bound = || show(counter)
puts bound()

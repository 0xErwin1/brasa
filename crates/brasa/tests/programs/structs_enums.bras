# Golden: structs, methods, field mutation, a custom toString override
# (top-level, interpolated, and nested), enums, match with guards,
# Option, `?.`, and `??`.

struct Point
  x: float
  y: float

  def norm(self): float
    self.x * self.x + self.y * self.y
  end
end

struct Money
  cents: int

  def toString(self): string
    "$#{self.cents / 100}.#{self.cents % 100}"
  end
end

enum Shape
  Circle(radius: float)
  Rect(w: float, h: float)
  Dot
end

def describe(shape: Shape): string
  match shape
    Circle(r) if r > 10.0 => "big circle"
    Circle(_) => "circle"
    Rect(w, h) if w == h => "square"
    Rect(_, _) => "rect"
    Dot => "dot"
  end
end

let p = Point { x: 3.0, y: 4.0 }
puts p
puts "norm: #{p.norm()}"
p.x = 6.0
puts p

let price = Money { cents: 250 }
puts price
puts "cost: #{price}"
let prices = [price, Money { cents: 999 }]
puts prices

let shapes = [Circle(12.0), Rect(2.0, 2.0), Dot]
for s in shapes
  puts describe(s)
end
puts shapes

let maybe: Option<int> = Some(41)
let missing: Option<int> = None
puts maybe ?? 0
puts missing ?? 7
puts maybe?.toString()

match maybe
  Some(v) => puts "got #{v + 1}"
  None => puts "nothing"
end
puts Some("hi")

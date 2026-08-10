# Structs, methods, enums, exhaustive match, and Option.

struct Point
  x: float
  y: float

  def dist(self, other: Point): float
    ((self.x - other.x) ** 2.0 + (self.y - other.y) ** 2.0).sqrt()
  end
end

enum Shape
  Circle(radius: float)
  Rect(w: float, h: float)
  Dot
end

def area(shape: Shape): float
  match shape
    Circle(r) => 3.14159 * r * r
    Rect(w, h) => w * h
    Dot => 0.0
  end
end

def describe(shape: Shape): string
  match shape
    Circle(r) if r > 100.0 => "huge circle"
    Circle(_) => "circle"
    Rect(w, h) if w == h => "square"
    Rect(_, _) => "rectangle"
    Dot => "dot"
  end
end

def biggest(shapes: Vector<Shape>): Option<Shape>
  shapes.sortBy(|s| -area(s)).first()
end

let shapes = [Circle(2.0), Rect(3.0, 3.0), Dot]

for s in shapes
  puts "#{describe(s)}: area #{area(s)}"
end

match biggest(shapes)
  Some(s) => puts "biggest is a #{describe(s)}"
  None => puts "no shapes"
end

let origin = Point { x: 0.0, y: 0.0 }
let p = Point { x: 3.0, y: 4.0 }
puts "distance: #{origin.dist(p)}"

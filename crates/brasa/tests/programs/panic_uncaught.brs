# Failure case: an uncaught panic reaches the top level with a call
# chain; a wildcard arm never catches a panic.

def inner(v: Vector<int>): int
  v[99] catch (e)
    _ => -1
  end
end

def outer(v: Vector<int>): int
  inner(v)
end

puts "start"
puts outer([1, 2, 3])

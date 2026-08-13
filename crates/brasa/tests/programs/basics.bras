# Golden: arithmetic, interpolation, control flow, ranges, recursion,
# numeric conversions, and float formatting.

def fib(n: int): int
  if n < 2 then n else fib(n - 1) + fib(n - 2) end
end

let mut total = 0
for i in 1..=5
  total = total + i
end
puts "total: #{total}"

let mut n = 3
while n > 0
  puts "tick #{n}"
  n = n - 1
end

for i in 0..4
  puts "fib(#{i}) = #{fib(i)}"
end

let x = 7
if x % 2 == 0
  puts "even"
elsif x % 3 == 0
  puts "odd multiple of 3"
else
  puts "odd"
end

puts "quarter: #{1.0 / 4.0}"
puts "big: #{2 ** 62}"

let truncated = 3.9
puts truncated.toInt()
let widened = 2
puts widened.toFloat()

print "no"
print "newline"
puts ""
puts 0..10
puts 1..=3

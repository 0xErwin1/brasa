# Recursive and iterative Fibonacci.

def fib(n: int): int
  if n < 2 then n else fib(n - 1) + fib(n - 2) end
end

def fibIter(n: int): int
  let mut a = 0
  let mut b = 1

  for _ in 0..n
    let next = a + b
    a = b
    b = next
  end

  a
end

for i in 0..=10
  puts "fib(#{i}) = #{fib(i)}"
end

puts "iterative fib(40) = #{fibIter(40)}"

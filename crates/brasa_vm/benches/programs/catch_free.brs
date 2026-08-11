struct Failure
  code: int
end

def step(n: int): int
  if n < 0
    throw Failure { code: n }
  end
  n + 1
end

def work(): int
  let mut total = 0
  let mut i = 0
  while i < 50000
    let v = step(i)
    total = total + v
    i = i + 1
  end
  total
end

puts work()

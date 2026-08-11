def makeAdder(k: int): (int) -> int
  |n| n + k
end

def work(): int
  let mut total = 0
  let mut i = 0
  while i < 20000
    let add = makeAdder(i)
    let twice = |x: int| add(x) + add(x)
    total = total + twice(i)
    i = i + 1
  end
  total
end

puts work()

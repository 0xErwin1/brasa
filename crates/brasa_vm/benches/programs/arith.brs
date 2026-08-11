def work(): int
  let mut total = 0
  let mut i = 0
  while i < 100000
    total = total + i * 3 % 7 - 1
    i = i + 1
  end
  total
end

puts work()

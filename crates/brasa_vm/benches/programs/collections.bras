def work(): int
  let mut nums = [0]
  let mut i = 1
  while i < 5000
    nums.push(i)
    i = i + 1
  end

  let tripled = nums.map(|n| n * 3)
  let picked = tripled.filter(|n| n % 2 == 0)

  let mut sum = 0
  for n in picked
    sum = sum + n
  end

  let index: Map<string, int> = { "seed": 0 }
  let mut k = 0
  while k < 300
    index.insert("key#{k}", k)
    k = k + 1
  end

  for (name, v) in index
    sum = sum + v + name.len()
  end
  sum
end

puts work()

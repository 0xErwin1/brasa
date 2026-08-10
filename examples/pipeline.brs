# Collections, lambdas, and the pipe operator.

struct Repo
  name: string
  stars: int
end

def topNames(repos: Vector<Repo>, min: int): Vector<string>
  repos
    |> filter(|r| r.stars >= min)
    |> sortBy(|r| -r.stars)
    |> map(|r| r.name)
end

let repos = [
  Repo { name: "brasa", stars: 1 },
  Repo { name: "ignis", stars: 120 },
  Repo { name: "dbflux", stars: 48 },
]

for name in topNames(repos, 10)
  puts name
end

# Maps index to Option; ?? provides the default.
let stars: Map<string, int> = { "brasa": 1, "ignis": 120 }
puts "brasa: #{stars["brasa"] ?? 0}"
puts "unknown: #{stars["nope"] ?? 0}"

# Method chains work without pipes too; pipes shine mixing free
# functions and methods.
let total = repos.map(|r| r.stars).reduce(0, |acc, s| acc + s)
puts "total stars: #{total}"

# do-blocks for multiline lambdas.
repos.each do |r|
  let tag = if r.stars > 100 then "hot" else "warm" end
  puts "#{r.name} is #{tag}"
end

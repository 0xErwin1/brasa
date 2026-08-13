# Failure case: an uncaught throw propagates to the top level and ends
# the program with the error message and a non-zero exit
# (`docs/spec/04-errors.md`).

struct BoomError
  code: int
end

def go()
  throw BoomError { code: 7 }
end

puts "before"
go()
puts "after"

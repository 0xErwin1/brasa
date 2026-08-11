# Golden: throw + catch with narrowing arms, wildcard, guards, rethrow
# wrapping, and panics caught only by their named arm.

struct NetError
  detail: string
end

struct ParseError
  line: int
end

struct WrapError
  cause: string
end

def fetch(ok: bool): string
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end

def parse(flag: int): int
  if flag == 0
    throw ParseError { line: 3 }
  end
  flag
end

# Narrowing: the arm names the thrown type.
let page = fetch(false) catch (e)
  NetError => "recovered: #{e.detail}"
end
puts page

# Wildcard catches any error not matched earlier.
let n = parse(0) catch (e)
  NetError => -1
  _ => -2
end
puts "parsed: #{n}"

# Guards run after the type matches.
let g = parse(0) catch (e)
  ParseError if e.line > 10 => -10
  ParseError => e.line
end
puts "guarded: #{g}"

# Rethrow wrapping: a plain throw inside an arm propagates outward.
def load(ok: bool): string
  fetch(ok) catch (e)
    NetError => throw WrapError { cause: e.detail }
  end
end

let result = load(false) catch (e)
  WrapError => "wrapped: #{e.cause}"
end
puts result

# Panics are a separate channel: only the named arm catches them.
let items = [1, 2, 3]
let out = items[10] catch (e)
  panics.IndexOutOfBounds => 0
end
puts "out: #{out}"

def divide(a: int, b: int): int
  a / b
end

let d = divide(10, 0) catch (e)
  panics.DivisionByZero => -1
end
puts "div: #{d}"

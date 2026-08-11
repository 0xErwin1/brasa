# The error system: throw values, inferred error sets, catch as a
# non-exhaustive match, named panic capture.

struct NetError
  detail: string
end

struct ParseError
  line: int
end

def fetchPage(ok: bool): string
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end

# Optional, verified contract: the compiler checks this body can only
# throw what it declares.
def fetchStrict(url: string): string throws NetError
  fetchPage(url.len() > 0)
end

# Handle one error, let the rest propagate automatically.
let page = fetchPage(false) catch (e)
  NetError => "recovered: #{e.detail}"
end
puts page

# catch!: the compiler requires every inferred error to be handled.
def parse(s: string): int
  if s.len() == 0
    throw ParseError { line: 1 }
  end
  s.toInt()
end

let n = parse("42") catch! (e)
  ParseError => -1
  _ => -2
end
puts "parsed: #{n}"

# Panics are a separate channel: only a named arm catches them.
let items = [1, 2, 3]
let out = items[10] catch (e)
  panics.IndexOutOfBounds => 0
end
puts "out of range gives #{out}"

# Rethrow wrapping: a normal throw inside an arm.
struct ConfigError
  cause: string
end

def loadConfig(ok: bool): string
  fetchPage(ok) catch (e)
    NetError => throw ConfigError { cause: e.detail }
  end
end

# A module: everything is private unless pub.

pub def slugify(s: string): string
  s.trim().toLower().replace(" ", "-")
end

pub def shout(s: string): string
  decorate(s.toUpper())
end

def decorate(s: string): string
  ">> #{s} <<"
end

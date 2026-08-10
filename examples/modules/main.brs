# File imports bind by stem; top-level runs first, then main().

import "utils.brs"

let banner = utils.shout("brasa modules")

def main()
  puts banner
  puts utils.slugify("Hola Mundo Brasa")
end

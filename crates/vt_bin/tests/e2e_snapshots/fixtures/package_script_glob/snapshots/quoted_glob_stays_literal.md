# quoted_glob_stays_literal

Quoted pathname patterns remain literal instead of being expanded.

## `vt run quoted-glob`

```
$ vtt print "packages/*/src" ⊘ cache disabled
packages/*/src
```

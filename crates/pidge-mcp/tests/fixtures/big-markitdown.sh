#!/bin/sh
# Prints 3 MiB of output: more than pidge accepts from a conversion.
head -c 3145728 /dev/zero | tr '\0' 'x'

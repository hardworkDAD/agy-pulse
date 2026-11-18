#!/usr/bin/env node
'use strict';

const pulse = require('./agy-pulse.js');

if (require.main === module) {
  pulse.main();
}

module.exports = pulse;
